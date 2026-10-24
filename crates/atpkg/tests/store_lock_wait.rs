// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cross-process `store.lock` contention and `--wait-lock` CLI tests.

#![cfg(unix)]

use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::{Child, Command, Stdio};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn waiting_line() -> String {
    format!("atpkg: {}", atpkg::cli::LOCK_WAITING_MARKER)
}

fn acquired_line() -> String {
    format!("atpkg: {}", atpkg::cli::LOCK_ACQUIRED_MARKER)
}

const DECLINED_SENTENCE: &str = "removed on this machine";

struct Fixture {
    _root: aterm_tempfile::TempDir,
    home: PathBuf,
    config_home: PathBuf,
    registry: PathBuf,
    prefix: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-lock-wait-{case}-"))
            .tempdir()
            .unwrap();
        let home = root.path().join("home");
        let config_home = root.path().join("config");
        let registry = root.path().join("registry");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&registry).unwrap();
        let prefix = atpkg::store::default_prefix(&home);
        assert!(
            prefix.starts_with(&home),
            "fixture: the default prefix must sit under the temp HOME: {}",
            prefix.display()
        );
        Self {
            _root: root,
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

    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("ATPKG_REGISTRY", format!("dir:{}", self.registry.display()))
            .env(atpkg::lock::WAIT_ANNOUNCE_GRACE_SEAM, "0")
            .env_remove("ATERM_STATE_HOME")
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

const ANNOUNCE_WITHIN: Duration = Duration::from_secs(60);

fn no_progress_marker(stdout: &[String]) {
    for line in stdout {
        assert!(
            line.starts_with("atpkg: machine")
                || line.starts_with("atpkg machine:")
                || line.starts_with("atpkg: Universal Control"),
            "a refused pass prints only what it did to the host: {line:?}"
        );
    }
    for marker in [
        atpkg::cli::LOCK_WAITING_MARKER,
        atpkg::cli::NET_STARTING_MARKER,
        atpkg::cli::NET_INSTALLED_MARKER,
        atpkg::cli::SEED_FAILED_MARKER,
    ] {
        assert!(
            !stdout.iter().any(|l| l.contains(marker)),
            "no {marker:?} for a typed verb: {stdout:?}"
        );
    }
}

#[test]
fn a_conditional_link_waits_and_rechecks_the_selection_under_the_lock() {
    fn inventory(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        fn visit(root: &Path, path: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            let meta = std::fs::symlink_metadata(path).unwrap();
            let mut bytes = Vec::new();
            if meta.file_type().is_symlink() {
                bytes.push(b'L');
                bytes.extend_from_slice(
                    std::fs::read_link(path)
                        .unwrap()
                        .as_os_str()
                        .as_encoded_bytes(),
                );
            } else if meta.is_dir() {
                bytes.push(b'D');
                for entry in std::fs::read_dir(path).unwrap() {
                    visit(root, &entry.unwrap().path(), out);
                }
            } else {
                assert!(meta.is_file());
                bytes.push(b'F');
                bytes.extend(std::fs::read(path).unwrap());
            }
            out.push((path.strip_prefix(root).unwrap().to_path_buf(), bytes));
        }
        let mut out = Vec::new();
        visit(root, root, &mut out);
        out.sort();
        out
    }
    for (selection_changes, candidate_missing) in [(false, false), (true, false), (false, true)] {
        let fx = Fixture::new("conditional-link");
        let layout = fx.layout();
        let guard = fx.hold();
        let checkout = |name| {
            let path = fx.home.join(name);
            std::fs::create_dir_all(path.join("bin")).unwrap();
            std::fs::write(path.join("bin/codex"), b"fixture\n").unwrap();
            path
        };
        let old = checkout("old");
        let new = checkout("new");
        let manual = checkout("manual");
        if candidate_missing {
            std::fs::remove_file(new.join("bin/codex")).unwrap();
        }
        let bins = [PathBuf::from("bin/codex")];
        atpkg::link(&layout, "codex", &old, &bins).unwrap();
        let args = [
            "link",
            "codex",
            new.to_str().unwrap(),
            "bin/codex",
            "--if-checkout",
            old.to_str().unwrap(),
            "--if-prefix",
            fx.prefix.to_str().unwrap(),
        ];
        let (status, _, _, _) = stream(fx.spawn(&args)).finish(ANNOUNCE_WITHIN);
        assert_eq!(status.code(), Some(i32::from(atpkg::lock::CONTENDED_EXIT)));
        assert_eq!(
            atpkg::linkmode::linked_checkout(&layout, "codex"),
            Some(old.clone())
        );
        let mut waiting_args = args.to_vec();
        waiting_args.extend(["--wait-lock", "60"]);
        let mut child = stream(fx.spawn(&waiting_args));
        child.wait_for_line(&waiting_line(), ANNOUNCE_WITHIN);
        assert_eq!(
            atpkg::linkmode::linked_checkout(&layout, "codex"),
            Some(old.clone())
        );
        if selection_changes {
            atpkg::link(&layout, "codex", &manual, &bins).unwrap();
        }
        for dir in [layout.landing_dir(), layout.prefix.join("optin")] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("retained"), b"earlier pass evidence\n").unwrap();
        }
        let before = inventory(&layout.prefix);
        drop(guard);
        let (status, _, stdout, stderr) = child.finish(ANNOUNCE_WITHIN);
        let accepted = !selection_changes && !candidate_missing;
        assert_eq!(status.success(), accepted, "{stdout:?}; {stderr}");
        assert!(stdout.iter().any(|line| line.starts_with(&acquired_line())));
        assert_eq!(
            atpkg::linkmode::linked_checkout(&layout, "codex"),
            Some(if selection_changes {
                manual
            } else if accepted {
                new
            } else {
                old
            })
        );
        if accepted {
            assert!(!layout.landing_dir().exists());
            assert!(!layout.prefix.join("optin").exists());
        } else {
            assert_eq!(
                inventory(&layout.prefix),
                before,
                "a refused comparison must not start a pass"
            );
        }
    }
}

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
    let (status, _, stdout, stderr) = child.finish(Duration::from_secs(60));
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
    let acquired: Vec<usize> = stdout
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with(&acquired_line()))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        acquired.len(),
        1,
        "exactly one `lock-acquired:` line answers the wait: {stdout:?}"
    );
    assert!(
        waiting_at < acquired[0] && acquired[0] < ran_at,
        "announced, then acquired, then the verb: {stdout:?}"
    );
    assert!(
        !stderr.contains("holds the store lock"),
        "no refusal on stderr: {stderr}"
    );
}

#[test]
fn a_waiting_seed_times_out_with_exit_75() {
    let fx = Fixture::new("timeout");
    let _guard = fx.hold();
    let child = stream(fx.spawn(&["seed", "--wait-lock", "1"]));
    let (status, _, stdout, stderr) = child.finish(Duration::from_secs(60));
    assert_eq!(
        status.code(),
        Some(i32::from(atpkg::lock::CONTENDED_EXIT)),
        "a timed-out wait is the contention code: {status}; stderr: {stderr}"
    );
    let waiting_at = stdout
        .iter()
        .position(|l| l.starts_with(&waiting_line()))
        .unwrap_or_else(|| panic!("announced the wait: {stdout:?}"));
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
        !stdout.iter().any(|l| l.contains(DECLINED_SENTENCE)),
        "never ran the verb: {stdout:?}"
    );
    assert!(
        !stdout.iter().any(|l| l.starts_with(&acquired_line())),
        "a wait that ran out never claims the lock: {stdout:?}"
    );
    let lock = lock_path_of(&fx.prefix);
    assert!(
        stderr.contains(&lock),
        "the refusal names this fixture's lock: {stderr}"
    );
}

#[test]
fn a_typed_seed_stays_fail_fast_with_exit_75() {
    let fx = Fixture::new("typed");
    let _guard = fx.hold();
    let child = stream(fx.spawn(&["seed"]));
    let (status, _, stdout, stderr) = child.finish(Duration::from_secs(60));
    assert_eq!(
        status.code(),
        Some(i32::from(atpkg::lock::CONTENDED_EXIT)),
        "{status}; stderr: {stderr}"
    );
    no_progress_marker(&stdout);
}

#[test]
fn an_unwritable_prefix_never_waits_and_keeps_exit_1() {
    let fx = Fixture::new("badprefix");
    std::fs::create_dir_all(fx.prefix.parent().expect("a parent")).unwrap();
    std::fs::write(&fx.prefix, b"a file where the prefix directory should be").unwrap();
    let child = stream(fx.spawn(&["seed", "--wait-lock", "300"]));
    let (status, _, stdout, stderr) = child.finish(Duration::from_secs(60));
    assert_eq!(status.code(), Some(1), "{status}; stderr: {stderr}");
    no_progress_marker(&stdout);
}

fn pid_alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn an_orphaned_waiter_stands_down(case: &str, script: &str) {
    let fx = Fixture::new(case);
    let guard = fx.hold();
    let sh = fx
        .command("/bin/sh")
        .env_remove(atpkg::lock::WAIT_ANNOUNCE_GRACE_SEAM)
        .arg("-c")
        .arg(script)
        .arg("sh")
        .arg(env!("CARGO_BIN_EXE_atpkg"))
        .spawn()
        .expect("spawn the throwaway parent");
    let started = Instant::now();
    let streamed = stream(sh);
    let pid = loop {
        let found = streamed.stdout.lock().unwrap().iter().find_map(|l| {
            l.strip_prefix("waiter=")
                .and_then(|p| p.trim().parse::<u32>().ok())
        });
        if let Some(pid) = found {
            break pid;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "no waiter pid line within a minute: {:?}",
            streamed.stdout.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    while pid_alive(pid) {
        if started.elapsed() >= aterm_spec::wait::MUST_HAPPEN {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", &pid.to_string()])
                .status();
            panic!(
                "the orphaned waiter (pid {pid}) is still alive a minute into its \
                 {ORPHAN_BOUND_SECS} s bound: it is polling out its bound, not standing down"
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let gone_after = started.elapsed();
    let (status, _, stdout, stderr) = streamed.finish(Duration::from_secs(60));
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
    assert!(
        !stdout.iter().any(|l| l.starts_with(&waiting_line())),
        "no `lock-waiting:` announcement: {stdout:?}"
    );
    assert!(
        !stdout.iter().any(|l| l.contains(DECLINED_SENTENCE)),
        "the verb never ran: {stdout:?}"
    );
    drop(guard);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let refusal = match atpkg::lock::try_lock_store(&fx.layout()) {
            Ok(_) => break,
            Err(e) => e,
        };
        assert!(
            Instant::now() < deadline,
            "the lock is still not free a minute after the holder released it: {refusal:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn an_orphaned_waiter_stands_down_silently_before_its_bound() {
    an_orphaned_waiter_stands_down(
        "orphan",
        &format!(
            r#"ATPKG_SPAWNER_PID=$$ "$1" seed --wait-lock {ORPHAN_BOUND_SECS} & echo "waiter=$!""#
        ),
    );
}

const ORPHAN_BOUND_SECS: u64 = 600;

#[test]
fn a_waiter_whose_parent_is_init_from_the_start_is_not_an_orphan() {
    use std::io::Write as _;
    let fx = Fixture::new("init-parent");
    let _guard = fx.hold();
    let bound = aterm_spec::wait::MUST_HAPPEN;
    let gate_path = fx.home.with_file_name("gate");
    let gate_c = {
        use std::os::unix::ffi::OsStrExt as _;
        std::ffi::CString::new(gate_path.as_os_str().as_bytes()).expect("a path with no NUL")
    };
    // SAFETY: `gate_c` is a NUL-terminated path that outlives the call.
    let made = unsafe { libc::mkfifo(gate_c.as_ptr(), 0o600) };
    assert_eq!(made, 0, "mkfifo: {}", std::io::Error::last_os_error());
    let mut gate = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&gate_path)
        .expect("hold the gate");
    let sh = fx
        .command("/bin/sh")
        .arg("-c")
        .arg(r#"( read _ <"$3"; exec "$1" seed --wait-lock "$2" ) & echo "waiter=$!""#)
        .arg("sh")
        .arg(env!("CARGO_BIN_EXE_atpkg"))
        .arg(bound.as_secs().to_string())
        .arg(&gate_path)
        .spawn()
        .expect("spawn the throwaway parent");
    let sh_pid = sh.id();
    let mut streamed = stream(sh);
    let reaped = Instant::now();
    let exited = loop {
        if let Some(status) = streamed
            .child
            .try_wait()
            .expect("poll the throwaway parent")
        {
            break status;
        }
        assert!(
            reaped.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the throwaway parent (pid {sh_pid}) never exited"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(exited.success(), "the throwaway parent itself: {exited}");
    gate.write_all(b"go\n").expect("open the gate");
    let deadline = Instant::now() + ANNOUNCE_WITHIN;
    loop {
        let ended = streamed.stdout_reader.is_finished();
        if streamed.stdout_has(&waiting_line()) {
            break;
        }
        assert!(
            !ended,
            "no announcement — the waiter stood down: it exited with stdout {:?}",
            streamed.stdout.lock().unwrap()
        );
        assert!(
            Instant::now() < deadline,
            "no announcement and no exit within {ANNOUNCE_WITHIN:?}: {:?}",
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
    let ppid = parent_of(pid);
    assert!(
        ppid.is_some_and(|p| p != sh_pid),
        "the waiter (pid {pid}) is re-parented away from sh (pid {sh_pid}): ppid {ppid:?}"
    );
    let _ = Command::new("/bin/kill")
        .args(["-KILL", &pid.to_string()])
        .status();
    let _ = streamed.finish(Duration::from_secs(60));
}

fn parent_of(pid: u32) -> Option<u32> {
    #[cfg(target_os = "macos")]
    let parent = aterm_sysprobe::kproc::record(pid).map(|r| r.ppid);
    #[cfg(not(target_os = "macos"))]
    let parent = aterm_sysprobe::processes()?
        .procs
        .into_iter()
        .find(|p| p.pid == pid)
        .map(|p| p.ppid);
    parent
}

#[test]
fn a_declined_seed_without_contention_is_unchanged() {
    let fx = Fixture::new("uncontended");
    fx.decline();
    let child = stream(fx.spawn(&["seed", "--wait-lock", "30"]));
    let (status, _, stdout, stderr) = child.finish(Duration::from_secs(60));
    assert!(status.success(), "{status}; stderr: {stderr}");
    assert!(
        !stdout.iter().any(|l| l.starts_with(&waiting_line())),
        "the marker prints only on contention: {stdout:?}"
    );
    assert!(
        !stdout.iter().any(|l| l.starts_with(&acquired_line())),
        "…and a wait that was never announced is never answered: {stdout:?}"
    );
    assert!(
        stdout.iter().any(|l| l.contains(DECLINED_SENTENCE)),
        "the verb ran: {stdout:?}"
    );
}

const NOTHING_TO_UPDATE: &str = "nothing installed to update";

#[test]
fn a_waiting_update_stands_down_behind_a_failed_pass_that_ended_while_it_waited() {
    use aterm_update_core::pkg_check::PassOutcome;
    let fx = Fixture::new("behind-failed");
    let guard = fx.hold();
    let mut child = stream(fx.spawn(&["update", "--wait-lock", "60"]));
    child.wait_for_line(&waiting_line(), ANNOUNCE_WITHIN);
    let unix = aterm_time::unix_secs().unwrap();
    let ended = aterm_types::rfc3339::format_rfc3339(unix);
    let layout = fx.layout();
    let mut record = atpkg::status::read(&layout).unwrap_or_default();
    record.outcome = "update failed: the release host answered HTTP 503 (unavailable)".into();
    atpkg::status::write(&layout, &record).unwrap();
    atpkg::status::stamp_pass_end(&layout, &ended, PassOutcome::Failed).unwrap();
    drop(guard);
    let (status, _, stdout, stderr) = child.finish(Duration::from_secs(60));
    assert_eq!(
        status.code(),
        Some(1),
        "stdout: {stdout:?}; stderr: {stderr}"
    );
    assert!(
        stderr.contains("the last update failed") && stderr.contains("(unavailable)"),
        "the holder's failure, in its own sentence: {stderr}"
    );
    assert!(
        !stdout.iter().any(|l| l.contains(NOTHING_TO_UPDATE)),
        "the verb did not run: {stdout:?}"
    );
    let after = atpkg::status::read(&layout).expect("the holder's record");
    assert_eq!(
        (after.last_pass.as_str(), after.last_pass_at.as_str()),
        ("failed", ended.as_str()),
        "the record is the holder's"
    );
}

#[test]
fn a_waiting_update_stands_down_quietly_behind_a_pass_that_failed_only_for_its_cached_index() {
    use aterm_update_core::pkg_check::PassOutcome;
    let fx = Fixture::new("behind-cached-index");
    let guard = fx.hold();
    let mut child = stream(fx.spawn(&["update", "--wait-lock", "60"]));
    child.wait_for_line(&waiting_line(), ANNOUNCE_WITHIN);
    let unix = aterm_time::unix_secs().unwrap();
    let ended = aterm_types::rfc3339::format_rfc3339(unix);
    let layout = fx.layout();
    let mut record = atpkg::status::read(&layout).unwrap_or_default();
    record.outcome = format!(
        "up to date (index build 46) {}GitHub rate limit hit (HTTP 403))",
        atpkg::status::INDEX_FROM_CACHE_CLAUSE
    );
    atpkg::status::write(&layout, &record).unwrap();
    atpkg::status::stamp_pass_end_recorded(&layout, &ended, PassOutcome::Failed, 0, true).unwrap();
    drop(guard);
    let (status, _, stdout, stderr) = child.finish(Duration::from_secs(60));
    assert_eq!(
        status.code(),
        Some(1),
        "stdout: {stdout:?}; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("the last update failed"),
        "the holder said nothing on stderr, and neither does this: {stderr}"
    );
    assert!(
        stdout
            .iter()
            .any(|l| l.contains("the last update failed") && l.contains("index from cache")),
        "the line is said, on stdout: {stdout:?}"
    );
    let after = atpkg::status::read(&layout).expect("the holder's record");
    assert!(
        atpkg::status::stale_index_only_end(&after),
        "the record is the holder's: {after:?}"
    );
}

#[test]
fn an_update_with_nothing_to_check_records_its_end() {
    let fx = Fixture::new("nothing-to-check");
    fx.decline();
    let (status, _, stdout, stderr) = stream(fx.spawn(&["update"])).finish(Duration::from_secs(60));
    assert!(status.success(), "{status}; stderr: {stderr}");
    assert!(
        stdout.iter().any(|l| l.contains(NOTHING_TO_UPDATE)),
        "{stdout:?}"
    );
    let record = atpkg::status::read(&fx.layout()).expect("the pass's record");
    assert_eq!(record.last_pass, "ok");
    assert!(!record.last_pass_at.is_empty());
    assert!(record.last_success_at.is_empty(), "nothing was checked");
}

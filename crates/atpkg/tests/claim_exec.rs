// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The public claim wire, driven through the actual standalone CLI and private account ledger.

#![cfg(target_os = "macos")]

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use aterm_capacity::{Admit, Ledger, Live, Need, Pace};
use aterm_spec::wait::{MUST_HAPPEN, until_within};
use aterm_toolchain::record_lock::RecordLock;
use aterm_uds::spawnfd::{self, Child, Stdio};

const STATUS: i32 = 90;
const EXTRA: i32 = 91;
const FLOCK: i32 = 92;
const ACTOR: &str = "claim_exec_command_actor";
const ACTOR_OUT: &str = "ATPKG_CLAIM_ACTOR_OUT";
const ACTOR_FLOCK: &str = "ATPKG_CLAIM_ACTOR_FLOCK";

/// Probe the same open description without reopening its pathname. The original
/// stays live for the later release check; a successful shared guard unlocks on drop.
fn try_lock_same_description(file: &File) -> Result<RecordLock, std::fs::TryLockError> {
    let copy = aterm_uds::kept_out(|| file.try_clone()).map_err(std::fs::TryLockError::Error)?;
    RecordLock::try_lock(copy, 0)
}

struct Fixture {
    root: aterm_tempfile::TempDir,
    ledger: Ledger,
    cover: Option<String>,
}

impl Fixture {
    fn new() -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix("atpkg-claim-exec-")
            .tempdir()
            .unwrap();
        for dir in ["home", "config", "state", "bin", "checkout"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
        }
        let ledger = Ledger::at(root.path().join("ledger"));
        Self {
            root,
            ledger,
            cover: None,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn command(&self) -> spawnfd::Command {
        let mut command = spawnfd::command(aterm_tempfile::bin_exe("atpkg"));
        command
            .env_clear()
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("ATERM_STATE_HOME", self.path("state"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .env(atpkg::build_admit::LEDGER_ENV, self.ledger.dir())
            // Public claim must still make a normal guarded look, never force admission.
            .env(atpkg::build_admit::ADMIT_ENV, "now")
            .env(ACTOR_OUT, self.path("observed.json"))
            .current_dir(self.path("checkout"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(marker) = &self.cover {
            command.env(aterm_capacity::COVER_ENV, marker);
        }
        command
    }

    fn claim(&self, flags: &[&str], program: &Path, args: &[OsString], flock: bool) -> Running {
        let (reader, writer) = aterm_uds::pipe().unwrap();
        set_nonblocking(reader.as_raw_fd());
        let mut command = self.command();
        command.args([
            "claim",
            "--what",
            "public CLI fixture",
            "--key",
            "claim-fixture-v1",
            "--checkout",
        ]);
        command
            .arg(self.path("checkout"))
            .args([
                "--rank",
                "ordinary",
                "--cores",
                "1",
                "--mem-bytes",
                "0",
                "--status-fd",
                "90",
            ])
            .args(flags);
        let mut guards: Vec<(OwnedFd, i32)> = vec![(writer.into(), STATUS)];
        let extra = File::open(self.path("inherited.txt")).unwrap();
        guards.push((extra.into(), EXTRA));
        if flock {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.path("worktree.lock"))
                .unwrap();
            guards.push((file.into(), FLOCK));
            command.args(["--flock-fd", "92"]);
            command.env(ACTOR_FLOCK, self.path("worktree.lock"));
        }
        command.arg("--").arg(program).args(args);
        command.inherit_guards_at(guards).unwrap();
        let defaults =
            atpkg::platform::Starting::at_defaults(&[libc::SIGINT, libc::SIGTERM, libc::SIGHUP]);
        let child = defaults.spawning(|| command.spawn().expect("actual atpkg claim"));
        // In particular, the parent's copy of the worktree OFD must not keep its lock alive.
        drop(command);
        drop(defaults);
        Running {
            child: Some(child),
            status: Some(reader),
        }
    }

    fn actor(&self, flags: &[&str], flock: bool) -> Running {
        self.claim(
            flags,
            &std::env::current_exe().unwrap(),
            &[
                "--exact".into(),
                ACTOR.into(),
                "--nocapture".into(),
                "--test-threads=1".into(),
            ],
            flock,
        )
    }

    fn rows(&self) -> Vec<aterm_capacity::Record> {
        self.ledger.list().unwrap()
    }

    fn empty(&self) {
        until_within(MUST_HAPPEN, "the private claim and waiter release", || {
            self.rows().is_empty().then_some(())
        });
    }

    fn prepare(&self) {
        std::fs::write(self.path("inherited.txt"), b"same inherited description\n").unwrap();
    }

    fn observed(&self, run: &mut Running) -> aterm_json::Value {
        until_within(MUST_HAPPEN, "the same-PID command's observation", || {
            if let Ok(bytes) = std::fs::read(self.path("observed.json")) {
                return Some(aterm_json::from_slice(&bytes).expect("complete actor JSON"));
            }
            if run.child.as_mut().unwrap().exited().unwrap() {
                let status = run.finish(false);
                panic!("command ended before publishing its observation: {status}");
            }
            None
        })
    }
}

fn set_nonblocking(fd: i32) {
    // SAFETY: this live pipe descriptor is owned by the test; no pointer arguments.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert!(flags >= 0);
    // SAFETY: only this pipe reader's status flags change.
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
}

struct Running {
    child: Option<Child>,
    status: Option<std::io::PipeReader>,
}

impl Running {
    fn pid(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }

    /// EOF is required while PROGRAM is still parked, proving no holder/target retained status.
    fn status(&mut self) -> Vec<u8> {
        let reader = self.status.as_mut().unwrap();
        let mut bytes = Vec::new();
        until_within(MUST_HAPPEN, "one status line followed by EOF", || {
            let mut chunk = [0; 4097];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => return Some(()),
                    Ok(n) => {
                        bytes.extend_from_slice(&chunk[..n]);
                        assert!(bytes.len() <= 4096, "bounded public receipt");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return None,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => panic!("status read: {error}"),
                }
            }
        });
        bytes
    }

    fn finish(&mut self, release: bool) -> ExitStatus {
        let child = self.child.as_mut().unwrap();
        if release {
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(b"finish\n")
                .unwrap();
        }
        let status = until_within(MUST_HAPPEN, "the original command's positive wait", || {
            child.try_wait().unwrap()
        });
        let child = self.child.take().unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status, status);
        if !status.success() && status.code() != Some(23) && status.code() != Some(75) {
            panic!(
                "claim exited {status}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        status
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn receipt(bytes: &[u8], event: &str) -> aterm_json::Value {
    assert!(
        bytes.ends_with(b"\n"),
        "exactly one terminated record: {bytes:?}"
    );
    assert_eq!(bytes.iter().filter(|&&b| b == b'\n').count(), 1);
    let value: aterm_json::Value = aterm_json::from_slice(bytes).unwrap();
    let object = value.as_object().unwrap();
    let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(
        keys,
        if event == "admitted" {
            vec!["cores", "event", "marker", "mem_bytes", "rank", "v"]
        } else {
            vec!["cores", "event", "marker", "v"]
        }
    );
    assert_eq!(value["v"].as_u64(), Some(1));
    assert_eq!(value["event"].as_str(), Some(event));
    assert_eq!(value["cores"].as_u64(), Some(1));
    assert!(!value["marker"].as_str().unwrap().is_empty());
    value
}

#[test]
fn claim_exec_command_actor() {
    let Some(out) = std::env::var_os(ACTOR_OUT) else {
        return;
    };
    // SAFETY: this test-only actor owns the explicitly inherited EXTRA slot once, until return.
    let mut extra = unsafe { File::from_raw_fd(EXTRA) };
    let mut text = String::new();
    extra.read_to_string(&mut text).unwrap();
    let worktree = std::env::var_os(ACTOR_FLOCK).map(|path| {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: the actor owns the explicitly passed original worktree OFD at FLOCK.
        let file = unsafe { File::from_raw_fd(FLOCK) };
        let held = file.metadata().unwrap();
        let named = std::fs::metadata(path).unwrap();
        assert_eq!((held.dev(), held.ino()), (named.dev(), named.ino()));
        file
    });
    // SAFETY: F_GETFD only observes the status slot. It must have closed before exec.
    assert_eq!(unsafe { libc::fcntl(STATUS, libc::F_GETFD) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EBADF)
    );
    let value = aterm_json::json!({
        "pid": std::process::id(), "extra": text,
        "marker": std::env::var(aterm_capacity::COVER_ENV).unwrap(),
        "cargo": std::env::var("CARGO_BUILD_JOBS").unwrap(),
        "caller": std::env::var("TARGO_CALLER_JOBS").unwrap(),
        "planned": std::env::var("TARGO_PLANNED_JOBS").unwrap(),
    });
    let out = PathBuf::from(out);
    let pending = out.with_extension("pending");
    std::fs::write(&pending, aterm_json::to_vec(&value).unwrap()).unwrap();
    std::fs::rename(pending, out).unwrap();
    let mut release = String::new();
    std::io::stdin().read_line(&mut release).unwrap();
    assert_eq!(release, "finish\n");
    drop(worktree);
}

#[test]
fn public_claim_exec_preserves_pid_fds_and_worktree_lock_through_status_eof() {
    let fx = Fixture::new();
    fx.prepare();
    let mut capability = fx.command();
    capability.args(["claim", "--capabilities"]);
    let output = aterm_uds::deadline::output_within(&mut capability, MUST_HAPPEN).unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"aterm-claim-exec-v1\n");
    fx.empty();
    let mut run = fx.actor(&["--wait-ms", "30000"], true);
    let line = receipt(&run.status(), "admitted");
    let seen = fx.observed(&mut run);
    assert_eq!(seen["pid"].as_u64(), Some(u64::from(run.pid())));
    assert_eq!(seen["extra"].as_str(), Some("same inherited description\n"));
    assert_eq!(seen["marker"], line["marker"]);
    for key in ["cargo", "caller", "planned"] {
        assert_eq!(seen[key].as_str(), Some("1"));
    }
    assert_eq!(line["mem_bytes"].as_u64(), Some(0));
    assert_eq!(line["rank"].as_str(), Some("ordinary"));
    let rows = fx.rows();
    assert_eq!(rows.len(), 1, "one actual account claim: {rows:?}");
    assert_eq!(rows[0].pid, run.pid());
    assert!(rows[0].held.is_some());
    assert_eq!(rows[0].cover_marker(), line["marker"].as_str().unwrap());
    let competitor = OpenOptions::new()
        .read(true)
        .write(true)
        .open(fx.path("worktree.lock"))
        .unwrap();
    assert!(matches!(
        try_lock_same_description(&competitor),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    assert!(run.finish(true).success());
    fx.empty();
    drop(
        try_lock_same_description(&competitor)
            .expect("PROGRAM's exit releases its inherited worktree OFD"),
    );
}

#[test]
fn public_claim_uses_cover_without_an_extra_claim_and_preserves_raw_path_arguments() {
    let mut fx = Fixture::new();
    fx.prepare();
    let cover = aterm_capacity::admit_with(
        &fx.ledger,
        &Live,
        Need::new("public protocol covering parent")
            .cores(1)
            .mem(0)
            .covers_command(),
        Admit::Wait,
        Pace::default(),
        &mut |_| {},
    )
    .unwrap();
    fx.cover = Some(cover.record().cover_marker());
    let mut run = fx.actor(&["--wait-ms", "30000"], false);
    let line = receipt(&run.status(), "covered");
    let seen = fx.observed(&mut run);
    assert_eq!(seen["pid"].as_u64(), Some(u64::from(run.pid())));
    assert_eq!(
        line["marker"].as_str(),
        Some(cover.record().cover_marker().as_str())
    );
    assert_eq!(seen["marker"], line["marker"]);
    assert_eq!(fx.rows().len(), 1, "covered execution adds no claim");
    assert!(run.finish(true).success());
    drop(cover);
    fx.empty();

    let program = fx.path("bin/claim-command");
    // The target is found through PATH; the public front must never UTF-8-normalize argv.
    let raw = OsString::from_vec(b"raw-\xff-argument".to_vec());
    let args = [
        "space argument".into(),
        OsString::new(),
        raw.clone(),
        "--jobs=99".into(),
    ];
    // Reuse the private observation path for this separate shell's byte-exact argv record.
    std::fs::write(&program, b"#!/bin/sh\nprintf '%s\\000' \"$$\" \"$@\" >\"$ATPKG_CLAIM_ACTOR_OUT.pending\"\nmv \"$ATPKG_CLAIM_ACTOR_OUT.pending\" \"$ATPKG_CLAIM_ACTOR_OUT\"\nread line\nexit 23\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::remove_file(fx.path("observed.json")).unwrap();
    let mut run = fx.claim(
        &["--wait-ms", "30000"],
        Path::new("claim-command"),
        &args,
        false,
    );
    receipt(&run.status(), "admitted");
    let bytes = until_within(MUST_HAPPEN, "raw target argv", || {
        let bytes = std::fs::read(fx.path("observed.json")).ok()?;
        bytes.ends_with(b"\0").then_some(bytes)
    });
    let mut expected = run.pid().to_string().into_bytes();
    expected.push(0);
    for arg in &args {
        use std::os::unix::ffi::OsStrExt as _;
        expected.extend_from_slice(arg.as_bytes());
        expected.push(0);
    }
    assert_eq!(bytes, expected);
    assert_eq!(run.finish(true).code(), Some(23));
    fx.empty();
}

#[test]
fn public_claim_does_not_hold_capacity_while_the_inherited_worktree_flock_is_busy() {
    let fx = Fixture::new();
    fx.prepare();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(fx.path("worktree.lock"))
        .unwrap();
    let held = try_lock_same_description(&lock).unwrap();
    let mut refused = fx.actor(&["--no-wait"], true);
    assert_eq!(refused.finish(false).code(), Some(75));
    assert!(refused.status().is_empty());
    assert!(!fx.path("observed.json").exists());
    fx.empty();

    let mut waiting = fx.actor(&["--wait-ms", "30000"], true);
    until_within(MUST_HAPPEN, "the worktree-blocked public waiter", || {
        let rows = fx.rows();
        (rows.len() == 1 && rows[0].pid == waiting.pid() && rows[0].held.is_none()).then_some(())
    });
    assert!(
        !fx.path("observed.json").exists(),
        "no work starts with a busy worktree"
    );
    drop(held);
    receipt(&waiting.status(), "admitted");
    let seen = fx.observed(&mut waiting);
    assert_eq!(seen["pid"].as_u64(), Some(u64::from(waiting.pid())));
    assert!(matches!(
        try_lock_same_description(&lock),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    assert!(waiting.finish(true).success());
    fx.empty();
    drop(try_lock_same_description(&lock).unwrap());
}

#[test]
fn public_claim_refusal_reader_hup_and_signals_start_no_work_and_leave_no_waiter() {
    for case in [
        "no-wait",
        "timeout",
        "reader-hup",
        "sigint",
        "sighup",
        "sigterm",
    ] {
        let fx = Fixture::new();
        fx.prepare();
        let blocker = File::create(fx.path("worktree.lock")).unwrap();
        let held = try_lock_same_description(&blocker).unwrap();
        let flags: &[&str] = match case {
            "no-wait" => &["--no-wait"],
            "timeout" => &["--wait-ms", "1"],
            _ => &["--wait-ms", "30000"],
        };
        let mut run = fx.actor(flags, true);
        if !matches!(case, "no-wait" | "timeout") {
            until_within(MUST_HAPPEN, "the actual unadmitted public waiter", || {
                fx.rows()
                    .iter()
                    .any(|r| r.pid == run.pid() && r.held.is_none())
                    .then_some(())
            });
            if case == "reader-hup" {
                drop(run.status.take());
            } else {
                let signal = match case {
                    "sigint" => libc::SIGINT,
                    "sighup" => libc::SIGHUP,
                    _ => libc::SIGTERM,
                };
                // SAFETY: only the positively owned, unreaped claim front is signalled.
                assert_eq!(
                    unsafe { libc::kill(i32::try_from(run.pid()).unwrap(), signal) },
                    0
                );
            }
        }
        let status = run.finish(false);
        assert_eq!(status.code(), Some(75), "{case}: known not started");
        if run.status.is_some() {
            assert!(run.status().is_empty(), "{case}: no successful receipt");
        }
        assert!(
            !fx.path("observed.json").exists(),
            "{case}: PROGRAM never starts"
        );
        assert!(fx.rows().is_empty(), "{case}: the refused waiter left");
        drop(held);
        drop(blocker);
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(fx.path("worktree.lock"))
            .unwrap();
        drop(try_lock_same_description(&lock).expect("waiting never retains the worktree lock"));
        fx.empty();
    }
}

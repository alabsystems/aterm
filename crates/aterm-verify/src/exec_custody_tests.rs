// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Two direct-child controls; no descendant workload or process-table query.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

const HANG_DETECTOR: Duration = Duration::from_secs(60);
const CHILD_ROOT: &str = "ATERM_VERIFY_CUSTODY_CHILD_ROOT";
static TERM_SEEN: AtomicBool = AtomicBool::new(false);

extern "C" fn term_seen(_: i32) {
    TERM_SEEN.store(true, AtomicOrdering::SeqCst);
}

fn create_marker(path: &Path, bytes: &[u8]) -> io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)
}

fn child(root: &Path) {
    TERM_SEEN.store(false, AtomicOrdering::SeqCst);
    // SAFETY: the handler lives for this re-executed test process and only stores a lock-free
    // atomic; this child runs one exact test, then exits.
    let previous = unsafe { libc::signal(libc::SIGTERM, term_seen as extern "C" fn(i32) as usize) };
    assert_ne!(previous, libc::SIG_ERR, "install TERM handler");
    create_marker(&root.join("ready"), b"ready\n").expect("ready marker");
    let deadline = Instant::now() + HANG_DETECTOR;
    while !TERM_SEEN.load(AtomicOrdering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "custody child received no TERM or KILL"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    create_marker(&root.join("returned"), b"returned\n").expect("normal TERM return marker");
}

/// Own the exact Child before any ready read or assertion. Drop deregisters
/// Live before collecting a status, then kills only this retained direct child.
struct Owned {
    child: Option<std::process::Child>,
    live: Option<group::Live>,
    audit: Option<group::custody_audit::Scope>,
    directory: Option<aterm_tempfile::TempDir>,
    handed_off: bool,
}

impl Owned {
    fn start(alias: &str) -> io::Result<Self> {
        let directory = aterm_tempfile::Builder::new()
            .prefix("vg")
            .tempdir_in("/tmp")?;
        let command = Cmd::new(std::env::current_exe()?).args([
            "--exact",
            alias,
            "--nocapture",
            "--test-threads=1",
        ]);
        let env = ExecEnv {
            cwd: directory.path(),
            path: OsStr::new("/usr/bin:/bin"),
            scratch: directory.path(),
            child_ceiling: None,
            remove_env: &[],
            add_env: &[],
            stage_groups: None,
        };
        let mut command = spawn_command(&command, env, Path::new("/usr/bin/taskpolicy"));
        command
            .env(CHILD_ROOT, directory.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let spawning = group::Spawning::begin();
        let child = command.spawn()?;
        let mut owned = Self {
            child: Some(child),
            live: None,
            audit: None,
            directory: Some(directory),
            handed_off: false,
        };
        let pid = owned.child.as_ref().expect("owned child").id();
        eprintln!(
            "VERIFY_CUSTODY_OWNED pid={pid} root={}",
            owned.root().display()
        );
        owned.audit = Some(group::custody_audit::Scope::arm(pid));
        owned.live = Some(group::Live::enter(pid));
        drop(spawning);
        Ok(owned)
    }

    fn root(&self) -> &Path {
        self.directory.as_ref().expect("owned root").path()
    }

    fn ready(&mut self) -> io::Result<()> {
        let deadline = Instant::now() + HANG_DETECTOR;
        loop {
            match std::fs::read(self.root().join("ready")) {
                Ok(bytes) if bytes == b"ready\n" => return Ok(()),
                Ok(bytes) if b"ready\n".starts_with(&bytes) => {} // write_all may be in progress
                Ok(_) => return Err(io::Error::other("invalid ready marker")),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            if self
                .live
                .as_mut()
                .expect("owned Live")
                .exited(self.child.as_mut().expect("owned child"), false)?
            {
                return Err(io::Error::other("custody child exited before readiness"));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "custody ready hang detector",
                ));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn cleanup(&mut self) -> io::Result<()> {
        drop(self.live.take());
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Err(e) if e.raw_os_error() == Some(libc::ECHILD) => return Err(e),
            Err(e) => return self.hand_off(child, e),
            Ok(None) => {}
        }
        let kill_error = child.kill().err();
        let deadline = Instant::now() + HANG_DETECTOR;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return kill_error.map_or(Ok(()), Err),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.raw_os_error() == Some(libc::ECHILD) => return Err(e),
                Err(e) => return self.hand_off(child, e),
                Ok(None) => {}
            }
            if Instant::now() >= deadline {
                return self.hand_off(
                    child,
                    io::Error::new(io::ErrorKind::TimedOut, "custody cleanup hang detector"),
                );
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn hand_off(&mut self, mut child: std::process::Child, error: io::Error) -> io::Result<()> {
        self.handed_off = true;
        let directory = self.directory.take();
        aterm_uds::reaper::hand_off(move || {
            let _keep_directory = &directory;
            match child.try_wait() {
                Ok(Some(_)) => true,
                Err(e) if e.raw_os_error() == Some(libc::ECHILD) => true,
                _ => false,
            }
        });
        Err(error)
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if let Err(e) = self.cleanup() {
            eprintln!(
                "VERIFY_CUSTODY_CLEANUP_UNPROVEN handed_off={} error={e}",
                self.handed_off
            );
        }
    }
}

fn exercise(alias: &str, graceful: bool) {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        child(Path::new(&root));
        return;
    }
    let mut owned = Owned::start(alias).expect("owned direct child");
    owned.ready().expect("ready handshake");
    let pid = owned.child.as_ref().expect("owned child").id();
    let grace = graceful.then_some(HANG_DETECTOR);
    let note = kill_and_reap(
        owned.child.as_mut().expect("owned child"),
        owned.live.as_mut().expect("owned Live"),
        grace,
    );
    let status = owned
        .child
        .as_mut()
        .expect("owned child")
        .try_wait()
        .expect("cached wait")
        .expect("reaped status");
    let audit = owned.audit.as_ref().expect("audit").report();
    assert_eq!(
        audit.violations, 0,
        "VERIFY_CUSTODY_SIGNAL_WITHOUT_CHILD: {audit:?}"
    );
    assert!(audit.kills >= 1, "actual group KILL was checked");
    assert_eq!(audit.terms, usize::from(graceful));
    assert_eq!(
        group::exited(pid, false)
            .expect_err("leader was collected")
            .raw_os_error(),
        Some(libc::ECHILD)
    );
    if graceful {
        assert!(status.success(), "normal TERM return: {status:?}; {note}");
        assert_eq!(
            std::fs::read(owned.root().join("returned")).expect("TERM return marker"),
            b"returned\n"
        );
        assert!(note.contains("the child exited within it"), "{note}");
    } else {
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert_eq!(
            std::fs::read(owned.root().join("returned"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }
    assert!(owned.live.as_ref().expect("Live").term().is_err());
    assert!(owned.live.as_ref().expect("Live").kill().is_err());
    assert_eq!(owned.audit.as_ref().expect("audit").report(), audit);
    let cleanup = owned.cleanup();
    assert!(!owned.handed_off, "failed cleanup custody is not GREEN");
    cleanup.expect("owned cleanup");
    eprintln!("VERIFY_CUSTODY_REAPED pid={pid} graceful={graceful}");
}

#[test]
fn a_grace_exit_keeps_its_group_custody_until_reap() {
    exercise(
        "exec::custody_tests::a_grace_exit_keeps_its_group_custody_until_reap",
        true,
    );
}

#[test]
fn a_forced_exit_releases_group_custody_after_reap() {
    exercise(
        "exec::custody_tests::a_forced_exit_releases_group_custody_after_reap",
        false,
    );
}

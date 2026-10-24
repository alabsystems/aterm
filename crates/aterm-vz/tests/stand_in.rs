// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stand-in leftovers (`docs/DESIGN-vz-guest-2026-10-23.md` §13, §5.1's escalation): a
//! real process holding the helper's run files, escalated by the lab's own code. The
//! stand-in is this test binary in a stand-in mode ([`stand_in_mode`]), never the helper: it
//! takes the run files as the helper does ([`aterm_vz::helper::take`]), then accepts sessions
//! and never answers. It loads no framework and starts no VM, and the signed, entitled
//! helper carries no test mode.
//!
//! - A helper that does not answer is SIGKILLed by its recorded pid once that pid's unique
//!   id is the record's, and the run says NOT RUN naming it.
//! - So is one that closed its socket and does not exit, after a session's close or a
//!   `down`: no wait on a helper ends with it running and its record kept.
//! - Negative controls: a record whose unique id differs names a dead helper, so nothing is
//!   signalled; and `status`, which signals nothing whatever the helper answers. The
//!   stand-in lives on, and the test ends it.
//! - A killed helper's record and socket are leftovers the next `down` removes.
//! - A second helper for the same disk fails the `flock`, naming the holder.
//!
//! Every wait for a death or a line is a one-minute hang detector; only the lab's "answers
//! nothing" and "has not ended" spans are short, since nothing must happen within them.
#![cfg(target_vendor = "apple")]

use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Child, Stdio};
use aterm_vz::client::{ClientError, Lab, Start};
use aterm_vz::proto::{Attach, PROTO};
use aterm_vz::record::{Kernel, Paths, Pids, Record};

/// The hang detector for what must happen.
const HANG: Duration = Duration::from_secs(60);

/// How long the lab lets a stand-in answer nothing (it never answers).
const SILENT: Duration = Duration::from_millis(500);

/// The stand-in mode's switches: the state directory, and the pid of the test that started
/// it.
const STAND_IN_STATE: &str = "ATERM_VZ_STAND_IN_STATE";
const STAND_IN_PARENT: &str = "ATERM_VZ_STAND_IN_PARENT";

/// The content address a stand-in's record names (no `Attach` is ever answered).
const STAND_IN_SHA: &str = "5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e";

/// This binary as the stand-in helper (§13): with [`STAND_IN_STATE`] unset, an ordinary
/// run's pass; set, it takes the run files under that state as the helper does, says so,
/// then accepts sessions and answers none while the test that started it lives, and exits
/// (1, with the helper's words, when the run files are held).
#[test]
fn stand_in_mode() {
    let (Some(state), Some(parent)) = (
        std::env::var_os(STAND_IN_STATE),
        std::env::var(STAND_IN_PARENT)
            .ok()
            .and_then(|p| p.parse::<u32>().ok()),
    ) else {
        return;
    };
    std::process::exit(stand_in_main(Path::new(&state), parent));
}

fn stand_in_main(state: &Path, parent: u32) -> i32 {
    use std::io::Write as _;
    let paths = Paths::at(state);
    let ident = aterm_vz::helper::this_process(STAND_IN_SHA.into(), paths.live_disk());
    let held = match aterm_vz::helper::take(&paths, &ident, &Kernel) {
        Ok(held) => held,
        Err(e) => {
            eprintln!("aterm-vz stand-in: {e}");
            return 1;
        }
    };
    {
        // On a line of its own: libtest has written `test stand_in_mode ... ` with no newline.
        let mut out = std::io::stdout().lock();
        let _ = writeln!(
            out,
            "\nstand-in {} took {}",
            ident.pid,
            held.socket.display()
        );
        let _ = out.flush();
    }
    if let Err(e) = held.listener.set_nonblocking(true) {
        eprintln!("aterm-vz stand-in: {e}");
        return 1;
    }
    let mut open = Vec::new();
    while std::os::unix::process::parent_id() == parent {
        match held.listener.accept() {
            Ok((stream, _)) => open.push(stream),
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    drop(open);
    0
}

/// This binary, run again as the stand-in on `state`.
fn stand_in_command(state: &Path) -> aterm_uds::spawnfd::Command {
    let exe = std::env::current_exe().expect("this test binary's path");
    let mut cmd = aterm_uds::spawnfd::command(exe);
    cmd.env(STAND_IN_STATE, state)
        .env(STAND_IN_PARENT, std::process::id().to_string())
        .args([
            "--exact",
            "stand_in_mode",
            "--nocapture",
            "--test-threads=1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// A scratch state directory short enough for `sun_path` (`$TMPDIR` is not).
fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("vz")
        .tempdir_in("/tmp")
        .expect("a scratch folder short enough for sun_path")
}

/// A stand-in on `state`, once it wrote its line (its run files are taken by then).
fn stand_in(state: &Path) -> Child {
    let mut child = stand_in_command(state)
        .spawn()
        .expect("the stand-in starts");
    let line = format!(
        "stand-in {} took {}",
        child.id(),
        Paths::at(state).socket().display()
    );
    aterm_uds::deadline::prints(&mut child, &line, HANG);
    child
}

/// The lab's `Start` over a stand-in this test spawned: it starts nothing more, and it
/// kills and watches as the real one does (by the record's pid and unique id), reaping its
/// own child so a SIGKILLed stand-in is gone rather than a zombie.
struct Tracked {
    child: Mutex<Child>,
    ended: Mutex<Option<std::process::ExitStatus>>,
}

impl Tracked {
    fn new(child: Child) -> Self {
        Self {
            child: Mutex::new(child),
            ended: Mutex::new(None),
        }
    }

    fn reap(&self) -> Option<std::process::ExitStatus> {
        let mut ended = self.ended.lock().unwrap();
        if ended.is_none() {
            *ended = self.child.lock().unwrap().try_wait().ok().flatten();
        }
        *ended
    }

    /// The stand-in's exit, waited for within the hang detector.
    fn ended(&self) -> std::process::ExitStatus {
        let begun = Instant::now();
        loop {
            if let Some(status) = self.reap() {
                return status;
            }
            assert!(
                begun.elapsed() < HANG,
                "the stand-in did not end within a minute"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// End the stand-in (SIGKILL) and reap it, within the hang detector.
    fn end(&self) {
        if self.reap().is_none() {
            aterm_uds::deadline::kill_within(&mut self.child.lock().unwrap(), HANG);
        }
    }
}

impl Start for Tracked {
    fn start(&mut self, _paths: &Paths) -> Result<u32, String> {
        Err("this test starts no helper".into())
    }

    fn exited(&mut self) -> Option<String> {
        None
    }

    fn settled(&mut self) {}

    fn alive(&self, record: &Record) -> bool {
        if self.child.lock().unwrap().id() == record.pid && self.reap().is_some() {
            return false;
        }
        Kernel.uniq(record.pid) == Some(record.uniq)
    }

    fn kill(&mut self, record: &Record) -> Result<(), String> {
        aterm_vz::record::kill_helper(record, &Kernel)
    }

    fn pids(&self) -> &dyn Pids {
        &Kernel
    }
}

fn attach() -> Attach {
    Attach {
        proto: PROTO,
        helper_sha256: "ab".repeat(32),
        agent_sha256: "cd".repeat(32),
        shares: Vec::new(),
        keep: false,
    }
}

fn lab(paths: &Paths, child: Child) -> Lab<Tracked> {
    let mut lab = Lab::new(paths.clone(), Tracked::new(child));
    lab.span = SILENT;
    lab.end_span = SILENT;
    lab
}

#[test]
fn a_helper_that_does_not_answer_is_sigkilled_by_its_pid_once_its_unique_id_matches() {
    let dir = scratch();
    let paths = Paths::at(dir.path());
    let child = stand_in(dir.path());
    let pid = child.id();
    let record = Record::read(&paths.record()).expect("the stand-in's record");
    assert_eq!(record.pid, pid);
    assert_eq!(
        Kernel.uniq(pid),
        Some(record.uniq),
        "the record holds its unique id"
    );

    let mut lab = lab(&paths, child);
    let Err(ClientError::Hung(words)) = lab.session(&attach()) else {
        panic!("a helper that answers nothing is hung");
    };
    assert!(
        words.starts_with("NOT RUN — the helper did not answer Attach"),
        "{words}"
    );
    assert!(
        words.contains(&format!(
            "helper {pid} (unique id {}, started {})",
            record.uniq, record.started
        )),
        "the run names the helper: {words}"
    );
    assert!(
        words.ends_with("it was SIGKILLed by its pid, its unique id checked"),
        "{words}"
    );
    assert_eq!(lab.start.ended().signal(), Some(libc::SIGKILL));
    assert!(
        Record::read(&paths.record()).is_none(),
        "its record is gone"
    );
    assert!(!paths.socket().exists(), "its socket is gone");
}

#[test]
fn a_record_whose_unique_id_differs_is_a_dead_helper_and_nothing_is_signalled() {
    let dir = scratch();
    let paths = Paths::at(dir.path());
    let child = stand_in(dir.path());
    let pid = child.id();
    // The record now names a process the pid no longer is: a reused pid.
    let mut reused = Record::read(&paths.record()).expect("the stand-in's record");
    reused.uniq = reused.uniq.wrapping_add(1);
    reused.write(&paths.record()).unwrap();

    let mut lab = lab(&paths, child);
    let Err(ClientError::Start(why)) = lab.session(&attach()) else {
        panic!("the dead helper's leftovers go, and a new helper is started");
    };
    assert_eq!(why, "this test starts no helper");
    assert!(
        Record::read(&paths.record()).is_none(),
        "the leftover record went"
    );
    assert!(!paths.socket().exists(), "the leftover socket went");
    assert_eq!(lab.start.reap(), None, "pid {pid} was never signalled");
    assert!(Kernel.uniq(pid).is_some(), "and still runs");
    lab.start.end();
}

#[test]
fn a_killed_helpers_record_and_socket_are_leftovers_down_removes() {
    let dir = scratch();
    let paths = Paths::at(dir.path());
    let child = stand_in(dir.path());
    let mut lab = lab(&paths, child);
    lab.start.end();
    assert!(
        Record::read(&paths.record()).is_some(),
        "a SIGKILLed helper cleans nothing"
    );
    lab.down().expect("down over a dead helper");
    assert!(Record::read(&paths.record()).is_none());
    assert!(!paths.socket().exists());
}

#[test]
fn a_second_helper_for_the_same_disk_fails_the_flock_naming_the_holder() {
    let dir = scratch();
    let paths = Paths::at(dir.path());
    let first = stand_in(dir.path());
    let holder = Record::read(&paths.record()).expect("the first stand-in's record");

    let mut cmd = stand_in_command(dir.path());
    let second = aterm_uds::deadline::output_within(&mut cmd, HANG).expect("the second ends");
    let err = String::from_utf8_lossy(&second.stderr);
    assert_eq!(second.status.code(), Some(1), "{err}");
    assert!(
        err.contains(&format!(
            "is held by helper {} (unique id {}, started {})",
            holder.pid, holder.uniq, holder.started
        )),
        "the second names the holder: {err}"
    );
    assert!(
        !String::from_utf8_lossy(&second.stdout).contains(" took "),
        "the second took nothing"
    );
    assert_eq!(
        Record::read(&paths.record()),
        Some(holder),
        "the holder's record is untouched"
    );
    Tracked::new(first).end();
}

#[test]
fn a_helper_whose_socket_closed_and_that_does_not_exit_is_sigkilled_after_the_session_closed() {
    let dir = scratch();
    let paths = Paths::at(dir.path());
    let child = stand_in(dir.path());
    let pid = child.id();
    std::fs::remove_file(paths.socket()).unwrap();
    let mut lab = lab(&paths, child);
    let Err(ClientError::Hung(words)) = lab.session(&attach()) else {
        panic!("a helper that neither serves nor ends is escalated");
    };
    assert!(
        words.starts_with(
            "NOT RUN — its session closed, and the helper did not exit within 0.5 s: helper"
        ) && words.contains(&format!("helper {pid} (unique id"))
            && words.ends_with("it was SIGKILLed by its pid, its unique id checked"),
        "{words}"
    );
    assert_eq!(lab.start.ended().signal(), Some(libc::SIGKILL));
    assert!(
        Record::read(&paths.record()).is_none(),
        "its record is gone"
    );
}

#[test]
fn down_on_a_helper_whose_socket_closed_and_that_does_not_exit_sigkills_it() {
    let dir = scratch();
    let paths = Paths::at(dir.path());
    let child = stand_in(dir.path());
    std::fs::remove_file(paths.socket()).unwrap();
    let mut lab = lab(&paths, child);
    let Err(ClientError::Hung(words)) = lab.down() else {
        panic!("down escalates a helper that does not end");
    };
    assert!(
        words.starts_with("NOT RUN — its socket closed on Stop, and the helper did not exit"),
        "{words}"
    );
    assert_eq!(lab.start.ended().signal(), Some(libc::SIGKILL));
    assert!(Record::read(&paths.record()).is_none());
}

#[test]
fn status_on_a_helper_that_does_not_answer_signals_nothing() {
    let dir = scratch();
    let paths = Paths::at(dir.path());
    let child = stand_in(dir.path());
    let pid = child.id();
    let mut lab = lab(&paths, child);
    let Err(ClientError::Hung(words)) = lab.status() else {
        panic!("status reports a helper that does not answer");
    };
    assert!(
        words.starts_with("NOT RUN — the helper did not answer State for 0.5 s: helper")
            && words.ends_with("status signals nothing"),
        "{words}"
    );
    assert_eq!(lab.start.reap(), None, "pid {pid} was never signalled");
    assert!(
        Record::read(&paths.record()).is_some(),
        "its record is kept"
    );
    lab.start.end();
}

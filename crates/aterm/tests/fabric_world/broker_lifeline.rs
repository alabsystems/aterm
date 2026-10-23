// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A BROKER ENDS WITH ITS LAUNCHER, AND WHEN ITS SOCKET PATH IS GONE**
//! (`docs/DESIGN-leftover-processes-2026-10-16.md` §5.5): a real `aterm link
//! broker` process, started by [`harness::spawn_broker`] as every suite here
//! starts one.
//!
//! * THE LAUNCHER DIES: a launcher stand-in (this test binary, run again) starts
//!   a broker that holds its lifeline, says where the broker is, and dies by
//!   SIGKILL, running no cleanup. launchd (or init) adopts the broker, which reads
//!   the cut at its next wake, says so on its stderr, unlinks its socket and
//!   exits. One verify run killed this way left seven brokers serving for ten
//!   hours.
//! * THE PATH GOES: its lifeline held all along, a broker whose socket file is
//!   unlinked can no longer be reached, and it exits 0 at its next wake, saying
//!   why.
//! * A DROP PROVES WHAT IT ENDS: the end a fabric suite's drop gives the bridge
//!   its aterm started ([`harness::end_bridge`], §8.2) ends a process only once
//!   its launcher's pid version proves that aterm started it.
//!
//! The negative control of the first two — a lifeline still held and a path still named
//! keep a broker serving — is `aterm_link::cli`'s pure test of the decision; no
//! test here waits to see a broker NOT exit. Every wait is a one-minute hang
//! detector, and no file a child runs from is copied or removed.

use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};

use crate::harness;

/// Selects the launcher stand-in: set, this test binary is the launcher, and its
/// value is the scratch directory its broker serves in.
const LAUNCHER_ENV: &str = "ATERM_FABRIC_TEST_BROKER_LAUNCHER";

/// The launcher test's own name, which the stand-in is run with.
const LAUNCHER_TEST: &str = "broker_lifeline::a_broker_ends_itself_when_its_launcher_is_sigkilled";

/// A private directory under `/tmp`: short, because a Unix socket path must fit
/// `sun_path` (104 bytes on macOS) and a per-user `$TMPDIR` does not.
fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("atb-")
        .tempdir_in("/tmp")
        .expect("scratch dir")
}

/// An unguarded `aterm link broker <dir>/b.sock <dir>/b.log`, its stderr in
/// `<dir>/broker.err`, returned once it prints `listening`.
fn serve(dir: &Path) -> harness::BrokerChild {
    let err = std::fs::File::create(dir.join("broker.err")).expect("broker.err");
    let sock = dir.join("b.sock").to_string_lossy().into_owned();
    let log = dir.join("b.log").to_string_lossy().into_owned();
    let mut broker = harness::spawn_broker(dir, &[&sock, &log], |cmd| {
        cmd.stdout(aterm_uds::spawnfd::Stdio::piped()).stderr(err);
    });
    let first = harness::first_line_within(&mut broker.child, "its readiness line");
    assert_eq!(
        first.trim_end(),
        format!("listening {sock}"),
        "broker.err:\n{}",
        said(dir)
    );
    broker
}

/// What the broker in `dir` wrote on its stderr.
fn said(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("broker.err")).unwrap_or_default()
}

/// **A BROKER WHOSE LAUNCHER WAS SIGKILLED ENDS ITSELF.** The stand-in's death is
/// the only thing that cuts the lifeline: it holds the held end until the kernel
/// closes it. The broker, adopted once its launcher is gone, is judged by its pid
/// and start together (the number alone may name another process by then), and by
/// the socket it unlinked and the line it wrote.
#[test]
fn a_broker_ends_itself_when_its_launcher_is_sigkilled() {
    if let Some(dir) = std::env::var_os(LAUNCHER_ENV) {
        // THE LAUNCHER STAND-IN: it holds the broker's lifeline and dies by
        // SIGKILL, so nothing of its own lets go of it.
        let dir = PathBuf::from(dir);
        let broker = serve(&dir);
        let pid = i32::try_from(broker.child.id()).expect("pid");
        let start = aterm_uds::process::read_process_start(pid).expect("the broker's start");
        // A file, not stdout: libtest owns this process's stdout.
        std::fs::write(dir.join("ready"), format!("{pid} {}", start.boot.0)).expect("ready");
        aterm_uds::process::kill_self_now();
    }
    let scratch = scratch();
    let dir = scratch.path();
    // The system's first-exec wait on a fresh build is paid here, unclocked, so the
    // launcher's own launch of the broker is not timed by the wait below.
    harness::aterm_to_launch();
    let err = std::fs::File::create(dir.join("launcher.err")).expect("launcher.err");
    let mut launcher =
        aterm_uds::spawnfd::command(std::env::current_exe().expect("this test binary"));
    launcher
        .arg(LAUNCHER_TEST)
        .args(["--exact", "--nocapture", "--test-threads=1"])
        .env(LAUNCHER_ENV, dir)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(err);
    let mut launcher = launcher
        .spawn()
        .expect("run this test binary as the launcher");
    let died = harness::until("the launcher to start its broker and die", || {
        launcher.try_wait().ok().flatten()
    });
    let launcher_said = std::fs::read_to_string(dir.join("launcher.err")).unwrap_or_default();
    assert_eq!(
        died.signal(),
        Some(9),
        "the launcher died by SIGKILL ({died}); its stderr:\n{launcher_said}"
    );
    let ready = std::fs::read_to_string(dir.join("ready")).expect("the launcher's ready file");
    let (pid, boot) = ready
        .split_once(' ')
        .and_then(|(pid, boot)| Some((pid.parse::<i32>().ok()?, boot.parse::<u64>().ok()?)))
        .unwrap_or_else(|| panic!("`<pid> <start>`: {ready:?}"));
    let start = aterm_uds::process::BootClock(boot);
    let sock = dir.join("b.sock");
    harness::until(
        "the orphaned broker to read its lifeline cut and end",
        || {
            let gone =
                aterm_uds::process::read_process_start(pid).is_none_or(|now| now.boot != start);
            (gone && !sock.exists()).then_some(())
        },
    );
    let said = said(dir);
    let why = format!("aterm link broker: {}", aterm_link::cli::LIFELINE_CUT_WORDS);
    assert!(said.contains(&why), "it said why it ended:\n{said}");
}

/// **A BROKER WHOSE SOCKET FILE IS GONE ENDS ITSELF**, its lifeline held all along
/// (so the path is the only thing that changed): unlinked, the socket can no
/// longer be reached, and the broker exits 0 at its next wake with the line that
/// says so.
#[test]
fn a_broker_ends_itself_once_its_socket_path_is_gone() {
    let scratch = scratch();
    let dir = scratch.path();
    let mut broker = serve(dir);
    let sock = dir.join("b.sock");
    std::fs::remove_file(&sock).expect("unlink the broker's socket");
    let ended = harness::until("the broker to see its path gone at its next wake", || {
        broker.child.try_wait().ok().flatten()
    });
    let said = said(dir);
    assert!(ended.success(), "a clean stop exits 0: {ended}\n{said}");
    let why = format!("{} {}", sock.display(), aterm_link::cli::PATH_GONE_WORDS);
    assert!(said.contains(&why), "it said why it ended:\n{said}");
    // Held until here: the lifeline never moved.
    drop(broker.lifeline);
}

/// **A DROP ENDS ONLY THE BRIDGE ITS OWN ATERM STARTED** ([`harness::end_bridge`]):
/// every process of a run runs the same aterm and is born after the first one, so
/// neither fact proves a pid read from a dead bridge's `pid` file; the launcher's pid
/// version does. Two brokers stand in, both this process's children: told this
/// process launched it, the drop ends the one the file names (`end_proven` returns
/// once it is gone, so no wait is needed to see either answer). NEGATIVE CONTROL: told
/// the other broker launched it, the drop leaves it alone. macOS only: Linux reads no
/// identity yet (the leftover design's slice 7).
#[cfg(target_vendor = "apple")]
#[test]
fn a_drop_ends_only_a_process_its_launcher_started() {
    let (first, second) = (scratch(), scratch());
    let mut other = serve(first.path());
    let mut target = serve(second.path());
    let state = second.path().join("state");
    std::fs::create_dir(&state).expect("state dir");
    std::fs::write(state.join("pid"), target.child.id().to_string()).expect("pid file");
    harness::end_bridge(&state, harness::identity_of(other.child.id()));
    assert!(
        target.child.try_wait().expect("try_wait").is_none(),
        "a process another launcher did not start is left alone"
    );
    harness::end_bridge(&state, harness::identity_of(std::process::id()));
    let ended = target.child.try_wait().expect("try_wait");
    assert_eq!(
        ended.and_then(|status| status.signal()),
        Some(15),
        "the process its launcher started is ended by SIGTERM: {ended:?}\n{}",
        said(second.path())
    );
    let _ = other.child.kill();
    let _ = other.child.wait();
}

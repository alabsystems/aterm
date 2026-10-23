// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Broker lifeline and socket-path liveness tests (`aterm link broker`).

use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};

use crate::harness;

const LAUNCHER_ENV: &str = "ATERM_FABRIC_TEST_BROKER_LAUNCHER";

const LAUNCHER_TEST: &str = "broker_lifeline::a_broker_ends_itself_when_its_launcher_is_sigkilled";

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("atb-")
        .tempdir_in("/tmp")
        .expect("scratch dir")
}

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

fn said(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("broker.err")).unwrap_or_default()
}

#[test]
fn a_broker_ends_itself_when_its_launcher_is_sigkilled() {
    if let Some(dir) = std::env::var_os(LAUNCHER_ENV) {
        let dir = PathBuf::from(dir);
        let broker = serve(&dir);
        let pid = i32::try_from(broker.child.id()).expect("pid");
        let start = aterm_uds::process::read_process_start(pid).expect("the broker's start");
        std::fs::write(dir.join("ready"), format!("{pid} {}", start.boot.0)).expect("ready");
        aterm_uds::process::kill_self_now();
    }
    let scratch = scratch();
    let dir = scratch.path();
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
    drop(broker.lifeline);
}

/// A drop ends only the bridge its own aterm started ([`harness::end_bridge`]): read with this
/// process as its launcher, it ends the broker the file names; under a binary translator no drop
/// proves the aterm under test, and the test says it is not run. NEGATIVE CONTROL: read with the
/// other broker as its launcher, the drop leaves it alone.
#[test]
fn a_drop_ends_only_a_process_its_launcher_started() {
    if let Some(translator) = aterm_uds::process::translator() {
        println!(
            "NOT RUN under {translator}: a_drop_ends_only_a_process_its_launcher_started: the \
             kernel names the translator as the broker's executable, so the drop proves none"
        );
        return;
    }
    let (first, second) = (scratch(), scratch());
    let mut other = serve(first.path());
    let mut target = serve(second.path());
    let state = second.path().join("state");
    std::fs::create_dir(&state).expect("state dir");
    std::fs::write(state.join("pid"), target.child.id().to_string()).expect("pid file");
    harness::end_bridge(&state, harness::launcher_of(other.child.id(), &state));
    assert!(
        target.child.try_wait().expect("try_wait").is_none(),
        "a process another launcher did not start is left alone"
    );
    harness::end_bridge(&state, harness::launcher_of(std::process::id(), &state));
    // `end_proven` returns once the pid stops naming the broker, which can be
    // before its exit has finished and it can be reaped.
    let ended = harness::until("the proven broker to be reaped", || {
        target.child.try_wait().expect("try_wait")
    });
    assert_eq!(
        ended.signal(),
        Some(15),
        "the process its launcher started is ended by SIGTERM: {ended:?}\n{}",
        said(second.path())
    );
    let _ = other.child.kill();
    let _ = other.child.wait();
}

/// **WITH NO PID VERSIONS, ONLY THE CHILD READ WHILE ITS ATERM LIVED PROVES A BRIDGE**
/// ([`harness::bridge_proven`]): Linux keeps neither a pid version nor a launcher's, so two
/// missing versions must never read as a launch. A Linux-shaped bridge is proven by the child
/// read while its aterm lived, and by nothing else; an Apple-shaped one by the versions alone.
/// NEGATIVE CONTROL: compare the two versions' `Option`s directly, and a bridge with no
/// child read is proven by `None == None`.
#[test]
fn a_versionless_bridge_is_proven_only_by_the_child_read_while_its_aterm_lived() {
    use aterm_uds::process::{Birth, Identity, Lineage};
    let identity = |pid, birth, idversion| Identity {
        pid,
        birth: Birth(birth),
        idversion,
    };
    let bridge = |launcher_version, idversion| Lineage {
        identity: identity(4243, 51_832_760, idversion),
        uniqueid: 51_832_760,
        parent: 1,
        pgid: 4243,
        launcher_version,
        tty: None,
    };
    let linux = bridge(None, None);
    let aterm = Some(identity(4242, 51_832_700, None));
    let read = |child| harness::Launcher {
        identity: aterm,
        child,
    };
    assert!(harness::bridge_proven(
        &linux,
        true,
        &read(Some(linux.identity))
    ));
    assert!(
        !harness::bridge_proven(&linux, true, &read(None)),
        "no child read: two missing versions prove nothing"
    );
    assert!(
        !harness::bridge_proven(&linux, true, &read(Some(identity(4243, 51_900_000, None)))),
        "the pid read as another process's child"
    );
    assert!(
        !harness::bridge_proven(&linux, false, &read(Some(linux.identity))),
        "not the aterm under test"
    );
    let apple = bridge(Some(77), Some(9));
    let launched = harness::Launcher {
        identity: Some(identity(4242, 1_792_000_000_000_042, Some(77))),
        child: None,
    };
    assert!(harness::bridge_proven(&apple, true, &launched));
    let another = harness::Launcher {
        identity: Some(identity(4242, 1_792_000_000_000_042, Some(78))),
        child: None,
    };
    assert!(!harness::bridge_proven(&apple, true, &another));
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::Path;
use std::sync::mpsc;

use aterm_spec::wait::MUST_HAPPEN;

use super::{Phase, Progress};
use crate::{Check, Ctx, EnvSnapshot, Scope};

fn events(path: &Path) -> Vec<aterm_json::Value> {
    std::fs::read_to_string(path)
        .expect("the retained observations")
        .lines()
        .filter_map(|line| line.strip_prefix("verify-event: "))
        .map(|line| aterm_json::from_str(line).expect("one complete event"))
        .filter(|event: &aterm_json::Value| {
            matches!(
                event["event"].as_str(),
                Some("test-phase-started" | "test-phase-ended")
            )
        })
        .collect()
}

/// A blocked phase is visible before it ends. Returning and unwinding close their own scopes,
/// never add a check verdict, and concurrent observers cannot close one another's phase.
#[test]
fn live_phases_are_attributed_and_close_without_claiming_a_verdict() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("phases.events");
    let ctx = Ctx::new(
        dir.path().to_path_buf(),
        Scope::workspace(),
        EnvSnapshot::default(),
        dir.path().to_path_buf(),
    )
    .with_events(Some(crate::event_log::EventLog::open(&path).unwrap()));
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let during = std::thread::scope(|scope| {
        let log = ctx.events.as_ref();
        let worker = scope.spawn(move || {
            let mut progress = Progress::new(log, Check::Test);
            progress.enter(Phase::RecordingAdmission);
            ready_tx.send(()).unwrap();
            release_rx.recv_timeout(MUST_HAPPEN).unwrap();
            progress.enter(Phase::Recording);
            progress.enter(Phase::Recording);
        });
        ready_rx.recv_timeout(MUST_HAPPEN).unwrap();
        let during = events(&path);
        let status_during = std::fs::read_to_string(&path).map(|raw| crate::status::observe(&raw));
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut other = Progress::new(ctx.events.as_ref(), Check::Checkers);
            other.enter(Phase::Preparation);
            panic!("injected unwind");
        }));
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        assert!(caught.is_err());
        (during, status_during)
    });
    let (during, status_during) = during;
    let status_during = status_during.expect("the held observation was read");
    assert!(
        status_during.phase_ends.is_empty(),
        "START never supplies an END"
    );
    assert_eq!(during.len(), 1, "the held scope has not ended");
    assert_eq!(during[0]["event"].as_str(), Some("test-phase-started"));
    assert_eq!(during[0]["phase"].as_str(), Some("recording-admission"));
    let all = events(&path);
    let run = during[0]["test_run"].as_u64().unwrap();
    let held: Vec<_> = all
        .iter()
        .filter(|event| event["test_run"].as_u64() == Some(run))
        .collect();
    assert_eq!(held.len(), 4, "one pair per distinct phase");
    assert_eq!(held[1]["scope_exit"].as_str(), Some("advanced"));
    assert_eq!(held[1]["phase_sequence"].as_u64(), Some(1));
    assert_eq!(held[2]["phase_sequence"].as_u64(), Some(2));
    assert_eq!(held[3]["scope_exit"].as_str(), Some("returned"));
    let unwound: Vec<_> = all
        .iter()
        .filter(|event| event["test_run"].as_u64() != Some(run))
        .collect();
    assert_eq!(unwound.len(), 2);
    assert_eq!(unwound[1]["scope_exit"].as_str(), Some("unwound"));
    let status_after = crate::status::observe(&std::fs::read_to_string(&path).unwrap());
    assert_eq!(status_after.phase_end_records, 3);
    assert_eq!(status_after.phase_ends.len(), 3);
    assert!(status_after.checks.is_empty());
    assert_eq!(status_after.terminal_records, 0);
    let json = status_after.json();
    let mut exits: Vec<_> = json["phase_ends"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["scope_exit"].as_str().unwrap())
        .collect();
    exits.sort_unstable();
    assert_eq!(exits, ["advanced", "returned", "unwound"]);
    assert!(json["phase_ends"].as_array().unwrap().iter().all(|row| {
        row["seconds"]
            .as_f64()
            .is_some_and(|seconds| seconds.is_finite() && seconds >= 0.0)
            && row["conflict"].as_bool() == Some(false)
    }));
    for event in all {
        let attribution = crate::event_log::attribution(&event).expect("bound to invocation");
        assert_eq!(
            Some(attribution.id.as_str()),
            ctx.events.as_ref().unwrap().id()
        );
        assert!(event.get("outcome").is_none(), "a phase is not a verdict");
        assert_eq!(event["provisional"].as_bool(), Some(true));
        assert_eq!(event["sample_complete"].as_bool(), Some(false));
        if event["event"].as_str() == Some("test-phase-ended") {
            assert!(event["seconds"].as_f64().is_some_and(|s| s >= 0.0));
        }
    }
}

#[cfg(unix)]
const RECORDING_STANDIN: &str = "ATERM_VERIFY_RECORDING_PHASE_STANDIN";

/// Run the real recording path in an isolated process, with a recorder that waits for the
/// parent to release its FIFO. One arm deliberately omits the record; the other dispatches
/// an actual passing executable and must preserve one observer across the whole run.
#[cfg(unix)]
#[test]
fn recording_phase_standin() {
    let Some(dir) = std::env::var_os(RECORDING_STANDIN).map(std::path::PathBuf::from) else {
        return;
    };
    crate::standin_body(
        &dir.join("trustc"),
        "printf 'rustc 1.93.0 (trust)\\nbinary: rustc\\nhost: aarch64-apple-darwin\\nrelease: 1\\n'",
    );
    let targo = dir.join("targo");
    crate::standin_body(
        &targo,
        r#"case "$*" in
  *config*get*build.target*)
    echo 'error: config value `build.target` is not set' >&2
    exit 101 ;;
esac
printf entered > "$PHASE_ENTERED"
IFS= read -r release < "$PHASE_FIFO"
if [ "$PHASE_VALID_RECORD" = yes ]; then
  exec "$PHASE_RECORDER_EXE" --exact test_progress::tests::valid_record_fixture --nocapture
fi
printf '     Running unittests src/lib.rs (target/debug/deps/fixture-1)\n'
exit 0"#,
    );
    let mut ctx = Ctx::new(
        dir.clone(),
        Scope::workspace(),
        EnvSnapshot::default(),
        dir.join("scratch"),
    )
    .with_events(Some(
        crate::event_log::EventLog::open(&dir.join("phases.events")).unwrap(),
    ));
    ctx.tools.stage2_dir = dir.clone();
    let mut report = crate::Report::new("recording phase");
    crate::testrun::run_tests(
        &ctx,
        &mut report,
        "fixture",
        &crate::exec::Cmd::new(&targo)
            .env("PHASE_ENTERED", dir.join("entered"))
            .env("PHASE_FIFO", dir.join("release.fifo"))
            .env("PHASE_RECORDER_EXE", std::env::current_exe().unwrap()),
        Check::Test,
    );
    if std::env::var("PHASE_VALID_RECORD").as_deref() == Ok("yes") {
        let outcomes: Vec<_> = report.outcomes().collect();
        assert!(!outcomes.is_empty());
        assert!(
            outcomes
                .iter()
                .all(|(outcome, _)| *outcome == crate::Outcome::Ok),
            "{outcomes:?}\n{}",
            report.render()
        );
    } else {
        assert!(report.outcomes().any(|(outcome, label)| {
            outcome == crate::Outcome::Fail(crate::Severity::CouldNotRun)
                && label.contains("recording runner left no record")
        }));
    }
    std::fs::write(dir.join("done"), "").unwrap();
}

/// The fake Cargo recorder supplies a coherent executable, header and encoded record. The
/// executor runs this test binary's passing fixture; this helper is not the test's verdict.
#[cfg(unix)]
#[test]
fn valid_record_fixture() {
    let Some(dir) = std::env::var_os(RECORDING_STANDIN).map(std::path::PathBuf::from) else {
        return;
    };
    let records = std::fs::read_dir(dir.join("scratch"))
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("test-records.")
        })
        .unwrap()
        .path();
    let executable = std::env::current_exe().unwrap();
    let record = crate::testrun::Record {
        cwd: dir.clone(),
        argv: vec![
            executable.as_os_str().to_owned(),
            "--exact".into(),
            "test_progress::tests::passing_test_fixture".into(),
        ],
        env: vec![
            ("CARGO_PKG_NAME".into(), "phase-fixture".into()),
            (RECORDING_STANDIN.into(), dir.into_os_string()),
        ],
    };
    std::fs::write(records.join("000000.rec"), record.encode()).unwrap();
    let artifact = aterm_json::json!({
        "reason": "compiler-artifact", "package_id": "phase-fixture",
        "target": {"kind": ["test"], "crate_types": ["bin"], "name": "phase_fixture"},
        "profile": {"test": true}, "executable": executable, "fresh": true,
    });
    println!(
        "\n{artifact}\n     Running tests/phase_fixture.rs ({})",
        executable.display()
    );
}

#[cfg(unix)]
#[test]
fn passing_test_fixture() {
    if let Some(dir) = std::env::var_os(RECORDING_STANDIN).map(std::path::PathBuf::from) {
        std::fs::write(dir.join("executed"), "").unwrap();
    }
}

#[cfg(unix)]
struct Child(aterm_uds::spawnfd::Child);

#[cfg(unix)]
impl Drop for Child {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[cfg(unix)]
fn run_recording_fixture(valid: bool) -> Vec<aterm_json::Value> {
    use std::io::Write as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    use aterm_spec::wait::until_within;

    let dir = aterm_tempfile::tempdir().expect("scratch");
    let fifo = dir.path().join("release.fifo");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: c_path is NUL-terminated and names a file in this test's private directory.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "test_progress::tests::recording_phase_standin",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(RECORDING_STANDIN, dir.path())
        .env("PHASE_VALID_RECORD", if valid { "yes" } else { "no" })
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(std::fs::File::create(dir.path().join("stderr")).unwrap());
    let mut child = Child(command.spawn().expect("the isolated recording run"));
    until_within(MUST_HAPPEN, "the recording child entered", || {
        dir.path().join("entered").exists().then_some(())
    });
    let path = dir.path().join("phases.events");
    let during = events(&path);
    let mut release = until_within(MUST_HAPPEN, "the recording FIFO reader", || {
        std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .ok()
    });
    release.write_all(b"release\n").unwrap();
    drop(release);
    let status = until_within(MUST_HAPPEN, "the recording run returned", || {
        child.0.try_wait().ok().flatten()
    });
    assert!(
        status.success() && dir.path().join("done").exists(),
        "{status}: {}",
        std::fs::read_to_string(dir.path().join("stderr")).unwrap()
    );
    assert_eq!(dir.path().join("executed").exists(), valid);
    assert_eq!(during.last().unwrap()["phase"].as_str(), Some("recording"));
    assert_eq!(
        during.last().unwrap()["event"].as_str(),
        Some("test-phase-started")
    );
    events(&path)
}

/// The journal identifies a real blocked recording child before it exits. Releasing that
/// child closes recording, reaches planning and preserves the recorder refusal, not a pass.
#[cfg(unix)]
#[test]
fn a_blocked_recording_child_is_observable_before_it_returns() {
    let after = run_recording_fixture(false);
    assert_eq!(after.last().unwrap()["phase"].as_str(), Some("planning"));
    assert_eq!(
        after.last().unwrap()["scope_exit"].as_str(),
        Some("returned")
    );
    assert!(
        !after
            .iter()
            .any(|event| event["phase"].as_str() == Some("dispatch"))
    );
}

/// A successful recording hands the same diagnostic identity to execution and reporting.
/// Negative control: replacing that handoff with a new Progress splits the observed run.
#[cfg(unix)]
#[test]
fn a_recorded_run_keeps_its_observer_through_execution_and_reporting() {
    let after = run_recording_fixture(true);
    let started: Vec<_> = after
        .iter()
        .filter(|event| event["event"].as_str() == Some("test-phase-started"))
        .collect();
    assert_eq!(
        started
            .iter()
            .map(|event| event["phase"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "preparation",
            "recording-admission",
            "recording-setup",
            "recording",
            "planning",
            "execution-admission",
            "planning",
            "probing",
            "dispatch",
            "reporting"
        ]
    );
    assert!(
        after
            .iter()
            .all(|event| event["test_run"] == started[0]["test_run"])
    );
    assert_eq!(
        started
            .iter()
            .map(|event| event["phase_sequence"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        (1..=10).collect::<Vec<_>>()
    );
    assert_eq!(after.last().unwrap()["phase"].as_str(), Some("reporting"));
    assert_eq!(
        after.last().unwrap()["scope_exit"].as_str(),
        Some("returned")
    );
}

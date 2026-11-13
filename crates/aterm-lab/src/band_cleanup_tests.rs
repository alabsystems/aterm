// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::VecDeque;
use std::os::unix::process::ExitStatusExt as _;

fn result(code: i32) -> Output {
    Output {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: Vec::new(),
        stderr: b"refused".to_vec(),
    }
}
struct Fake {
    now: Instant,
    end: Instant,
    observed: VecDeque<Result<Registration, CleanupError>>,
    removed: Option<Result<Output, CleanupError>>,
    commands: Vec<&'static str>,
    expire_on_pause: bool,
}
impl Fake {
    fn new(observed: Vec<Result<Registration, CleanupError>>, code: i32) -> Self {
        let now = Instant::now();
        Self {
            now,
            end: now + Duration::from_secs(60),
            observed: observed.into(),
            removed: Some(Ok(result(code))),
            commands: Vec::new(),
            expire_on_pause: false,
        }
    }
}
impl Control for Fake {
    fn now(&mut self) -> Instant {
        self.now
    }
    fn registration(&mut self, _: &str, deadline: Instant) -> Result<Registration, CleanupError> {
        assert_eq!(deadline, self.end);
        self.commands.push("print");
        self.observed.pop_front().expect("expected observation")
    }
    fn bootout(&mut self, _: &str, deadline: Instant) -> Result<Output, CleanupError> {
        assert_eq!(deadline, self.end);
        self.commands.push("bootout");
        self.removed.take().expect("only one mutation")
    }
    fn pause(&mut self) -> Result<(), CleanupError> {
        self.commands.push("pause");
        if self.expire_on_pause {
            self.now = self.end;
        }
        Ok(())
    }
}

#[test]
fn absent_and_retirement_race_need_positive_evidence_without_repeating_mutation() {
    let mut absent = Fake::new(vec![Ok(Registration::Absent)], 1);
    let end = absent.end;
    remove_with(&mut absent, "owned", end).unwrap();
    assert_eq!(absent.commands, ["print"]);
    let mut race = Fake::new(vec![Ok(Registration::Present), Ok(Registration::Absent)], 1);
    let end = race.end;
    remove_with(&mut race, "owned", end).unwrap();
    assert_eq!(race.commands, ["print", "bootout", "print"]);
    let mut pending = Fake::new(
        vec![
            Ok(Registration::Present),
            Ok(Registration::Present),
            Ok(Registration::Absent),
        ],
        0,
    );
    let end = pending.end;
    remove_with(&mut pending, "owned", end).unwrap();
    assert_eq!(
        pending.commands,
        ["print", "bootout", "print", "pause", "print"]
    );
}

#[test]
fn successful_bootout_is_not_absence_and_all_observations_share_one_deadline() {
    let mut pending = Fake::new(
        vec![Ok(Registration::Present), Ok(Registration::Present)],
        0,
    );
    pending.expire_on_pause = true;
    let end = pending.end;
    assert_eq!(
        remove_with(&mut pending, "owned", end).unwrap_err().kind,
        CleanupFailure::Deadline
    );
    assert_eq!(pending.commands, ["print", "bootout", "print", "pause"]);
}

#[test]
fn unknown_probe_and_command_timeout_remain_failures() {
    let mut unknown = Fake::new(
        vec![Err(CleanupError::new(
            CleanupFailure::Unknown,
            "print",
            "permission refused",
        ))],
        0,
    );
    let end = unknown.end;
    assert_eq!(
        remove_with(&mut unknown, "owned", end).unwrap_err().kind,
        CleanupFailure::Unknown
    );
    assert_eq!(unknown.commands, ["print"]);
    let mut timeout = Fake::new(vec![Ok(Registration::Present)], 0);
    timeout.removed = Some(Err(CleanupError::expired("bootout")));
    let end = timeout.end;
    assert_eq!(
        remove_with(&mut timeout, "owned", end).unwrap_err().kind,
        CleanupFailure::Deadline
    );
    assert_eq!(timeout.commands, ["print", "bootout"]);
    let mut refused = Fake::new(
        vec![Ok(Registration::Present), Ok(Registration::Present)],
        1,
    );
    let end = refused.end;
    assert_eq!(
        remove_with(&mut refused, "owned", end).unwrap_err().kind,
        CleanupFailure::Unknown
    );
    assert_eq!(refused.commands, ["print", "bootout", "print"]);
}

/// The role a launchctl stand-in test's re-run of this binary plays.
const PROBE: &str = "ATERM_LAB_BAND_CLEANUP_PROBE";

/// This binary's `test` alone as `role`, as a command.
fn rerun(test: &str, role: &str) -> Command {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    cmd.args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(PROBE, role);
    cmd
}

/// A cleanup's command (launchctl's, here a stand-in) runs through the stop that asked for
/// the cleanup, and a second signal ends it, as it ends the cleanup's own waits.
#[test]
fn a_second_signal_ends_a_cleanups_command_and_the_stop_alone_does_not() {
    const TEST: &str =
        "band::cleanup::tests::a_second_signal_ends_a_cleanups_command_and_the_stop_alone_does_not";
    match std::env::var(PROBE).as_deref() {
        Ok("probe") => {
            crate::sys::start_signal(libc::SIGTERM, false);
            crate::stop::install();
            let me = i32::try_from(std::process::id()).expect("pid");
            crate::sys::kill(me, libc::SIGTERM).expect("the stop");
            assert_eq!(
                crate::stop::sleep(Duration::from_mins(1)),
                Err(crate::stop::Stopped(libc::SIGTERM)),
                "the stop is noted"
            );
            let _grace = crate::stop::grace();
            let deadline = Instant::now() + Duration::from_mins(1);
            let through = output("print", &mut rerun(TEST, "answers"), deadline);
            println!(
                "PROBE through the stop: {:?}",
                through.map(|o| o.status.success()).map_err(|e| e.kind)
            );
            crate::sys::kill(me, libc::SIGTERM).expect("the cut");
            assert_eq!(
                crate::stop::sleep(Duration::from_mins(1)),
                Err(crate::stop::Stopped(libc::SIGTERM)),
                "the cut is noted"
            );
            let cut = output("print", &mut rerun(TEST, "stalls"), deadline);
            println!(
                "PROBE after the cut: {:?} before_deadline={}",
                cut.map(|o| o.status.success()).map_err(|e| e.kind),
                Instant::now() < deadline
            );
            std::process::exit(0);
        }
        Ok("answers") => std::process::exit(0),
        Ok(_) => {
            std::thread::sleep(Duration::from_mins(2));
            std::process::exit(0);
        }
        Err(_) => {}
    }
    let out = aterm_uds::deadline::output_until(
        &mut rerun(TEST, "probe"),
        Instant::now() + Duration::from_mins(2),
    )
    .expect("the probe ends within the hang detector");
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.contains("PROBE through the stop: Ok(true)"),
        "the stop that asked for the cleanup does not end its command: {out}"
    );
    assert!(
        out.contains("PROBE after the cut: Err(Interrupted) before_deadline=true"),
        "a second signal ends the cleanup's command at once, not at its deadline: {out}"
    );
}

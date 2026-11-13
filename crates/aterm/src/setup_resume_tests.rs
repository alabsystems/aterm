// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{REPLACED, Resume, resume_with, stopped_by};
use std::collections::VecDeque;
use std::process::ExitCode;
use std::time::Duration;

/// Scripted reads, and what the loop did with them. Unscripted, the build is not replaced,
/// an end may end the job, and a worker ran no pass.
#[derive(Default)]
struct Fake {
    replaced: VecDeque<bool>,
    resumable: VecDeque<Result<Option<u64>, String>>,
    windows: VecDeque<bool>,
    ends: VecDeque<bool>,
    passes: VecDeque<bool>,
    did: Vec<String>,
}

impl Resume for Fake {
    fn replaced(&mut self) -> bool {
        self.replaced.pop_front().unwrap_or(false)
    }
    fn resumable(&mut self) -> Result<Option<u64>, String> {
        self.resumable.pop_front().expect("the loop reads no more")
    }
    fn window_open(&mut self) -> Result<bool, String> {
        Ok(self.windows.pop_front().expect("the loop reads no more"))
    }
    fn wait_for_windows(&mut self) -> Result<(), String> {
        self.did.push("wait".into());
        Ok(())
    }
    fn work(&mut self, revision: u64) -> bool {
        self.did.push(format!("work {revision}"));
        self.passes.pop_front().unwrap_or(false)
    }
    fn end(&mut self) -> Result<bool, String> {
        self.did.push("end".into());
        Ok(self.ends.pop_front().unwrap_or(true))
    }
    fn pause(&mut self, pause: Duration) {
        self.did.push(format!("pause {}", pause.as_secs()));
    }
    fn say(&mut self, _: String) {}
}

fn after(tries: u32) -> String {
    format!(
        "pause {}",
        aterm_cli::macos_doctor::PASS_RETRY_SCHEDULE
            .after(tries)
            .as_secs()
    )
}

/// The job works the owed run while no window runs setup, waits while one does, goes on with
/// a newer run a window admitted, and exits 0 once nothing is owed, removing its definition:
/// the one exit launchd does not start it again after.
#[test]
fn the_job_works_the_owed_run_around_windows_and_ends_once_nothing_is_owed() {
    let mut host = Fake {
        resumable: [Ok(Some(3)), Ok(Some(3)), Ok(Some(4)), Ok(None)].into(),
        windows: [true, false, false].into(),
        ..Fake::default()
    };
    assert_eq!(resume_with(&mut host), ExitCode::SUCCESS);
    assert_eq!(host.did, ["wait", "work 3", "work 4", "end"]);
}

/// A read that fails, and a worker that ended with its run still owed having run no pass,
/// wait the run's own pause, growing, before the next look; the job never gives up while a
/// run is owed.
#[test]
fn a_failed_read_or_an_unfinished_worker_waits_the_runs_own_pause() {
    let mut host = Fake {
        resumable: [
            Err("the records are being written".into()),
            Ok(Some(5)),
            Ok(Some(5)),
            Ok(Some(5)),
            Ok(None),
        ]
        .into(),
        windows: [false, false, false].into(),
        ..Fake::default()
    };
    assert_eq!(resume_with(&mut host), ExitCode::SUCCESS);
    assert_eq!(
        host.did,
        [
            after(1),
            "work 5".into(),
            after(2),
            "work 5".into(),
            "end".into()
        ]
    );
}

/// A worker that ran a pass starts the job's count afresh: the look after it waits the
/// shortest pause, not one grown by the failures before it. Before, the count only grew for
/// the job's life.
#[test]
fn a_worker_that_ran_a_pass_starts_the_jobs_count_afresh() {
    let mut host = Fake {
        resumable: [
            Err("the records are being written".into()),
            Err("the records are being written".into()),
            Ok(Some(5)),
            Ok(Some(5)),
            Ok(None),
        ]
        .into(),
        windows: [false, false].into(),
        passes: [true].into(),
        ..Fake::default()
    };
    assert_eq!(resume_with(&mut host), ExitCode::SUCCESS);
    assert_eq!(
        host.did,
        [after(1), after(2), "work 5".into(), after(1), "end".into()]
    );
}

/// An end that the records' writer kept from ending (profile.lock busy, or a run admitted
/// since) reads the records again rather than exiting 0, which launchd would not start the
/// job again after. Before, the job exited 0 there and a run admitted meanwhile had no job.
#[test]
fn an_end_the_records_kept_from_ending_reads_them_again() {
    let mut host = Fake {
        resumable: [Ok(None), Ok(Some(6)), Ok(None)].into(),
        windows: [false].into(),
        ends: [false, true].into(),
        ..Fake::default()
    };
    assert_eq!(resume_with(&mut host), ExitCode::SUCCESS);
    assert_eq!(
        host.did,
        ["end".into(), after(1), "work 6".into(), "end".into()]
    );
}

/// Once an update replaced the build the job runs, it ends with an exit launchd answers by
/// starting it again on the new build, before it reads or runs anything more. Before, it went
/// on working the successor's records with the old code until the login ended.
#[test]
fn a_replaced_build_ends_the_job_for_launchd_to_start_the_new_one() {
    let mut host = Fake {
        replaced: [false, true].into(),
        resumable: [Ok(Some(3))].into(),
        windows: [false].into(),
        ..Fake::default()
    };
    assert_eq!(resume_with(&mut host), ExitCode::from(REPLACED));
    assert_ne!(ExitCode::from(REPLACED), ExitCode::SUCCESS);
    assert_eq!(host.did, ["work 3"]);
}

/// A logout's SIGTERM (or a SIGHUP, a SIGINT) notes the stages the job cuts off before it
/// ends, and ends it with a status other than 0, whether the note was written or not.
#[test]
fn a_stop_signal_notes_the_stages_it_cuts_off_and_ends_other_than_by_exit_0() {
    for (signal, noted) in [(15, Ok(())), (1, Err("the ledger is busy".to_owned()))] {
        let mut asked = 0;
        let status = stopped_by(signal, || {
            asked += 1;
            noted.clone()
        });
        assert_eq!(asked, 1, "signal {signal}");
        assert_eq!(status, 128 + signal);
    }
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::RunError;
use aterm_uds::deadline::{CompleteOutputError, RunError as CollectorError};

#[cfg(unix)]
#[test]
fn host_collectors_accept_an_overflowing_deadline() {
    use super::{HostTools, Run, Tools};

    let mut run = Run::new(aterm_uds::os_path!("true"), Vec::<String>::new());
    run.deadline = Some(std::time::Duration::MAX);
    assert!(HostTools.run(&run).expect("ordinary collection").success());
    let complete = HostTools
        .run_complete(&run, 0)
        .expect("complete collection with an overflowing deadline");
    assert!(complete.success());
    assert!(complete.stdout.is_empty());
}

#[test]
fn collector_failures_keep_dispatch_phase_timeout_and_original_cause() {
    let cause = || std::io::Error::other("publisher-collector-marker");
    for (error, started) in [
        (CollectorError::Spawn(cause()), false),
        (CollectorError::Capture(cause()), true),
        (CollectorError::Wait(cause()), true),
    ] {
        let why = error.to_string();
        let mapped = RunError::from_collector(error);
        assert_eq!(
            mapped,
            if started {
                RunError::AfterStart(why)
            } else {
                RunError::Spawn(why)
            }
        );
    }
    assert_eq!(
        RunError::from_collector(CollectorError::TimedOut),
        RunError::TimedOut
    );
}

#[test]
fn complete_collection_failure_keeps_dispatch_phase_and_timeout() {
    let error = CompleteOutputError::Run(CollectorError::Spawn(std::io::Error::other(
        "publisher-spawn-marker",
    )));
    let why = error.to_string();
    assert_eq!(
        RunError::from_complete_collector(error),
        RunError::Spawn(why)
    );
    assert_eq!(
        RunError::from_complete_collector(CompleteOutputError::Run(CollectorError::TimedOut)),
        RunError::TimedOut
    );
    for error in [
        CompleteOutputError::Run(CollectorError::Capture(std::io::Error::other(
            "publisher-capture-marker",
        ))),
        CompleteOutputError::Run(CollectorError::Wait(std::io::Error::other(
            "publisher-wait-marker",
        ))),
        CompleteOutputError::StdoutRead(std::io::Error::other("publisher-read-marker")),
        CompleteOutputError::StdoutTooLarge { limit: 3 },
        CompleteOutputError::StdoutIncomplete,
    ] {
        let why = error.to_string();
        assert_eq!(
            RunError::from_complete_collector(error),
            RunError::AfterStart(why)
        );
    }
}

/// A run that carries a queue is still ended at its bound on this host: no claim of its
/// checkout waits (the checkout is this test's own scratch folder, which no build names), so
/// every second is the run's own. The sleep is a hang detector's subject, far past the bound;
/// a run within its bound is collected whole, under either collector.
#[cfg(unix)]
#[test]
fn a_host_run_whose_queue_never_waits_is_ended_at_its_bound() {
    use super::{HostTools, Queue, Run, Tools};

    let scratch = aterm_tempfile::TempDir::new().expect("scratch");
    let queue = Queue::of(scratch.path());
    let bounded = |program: &str, args: &[&str], within: u64| {
        let mut run = Run::new(program, args.to_vec());
        run.deadline = Some(std::time::Duration::from_secs(within));
        run.queue = Some(queue.clone());
        run
    };
    let quick = bounded(aterm_uds::os_path!("true"), &[], 600);
    assert!(HostTools.run(&quick).expect("collected").success());
    assert!(HostTools.run_complete(&quick, 0).expect("whole").success());
    let hung = bounded(aterm_uds::os_path!("sleep"), &["600"], 1);
    assert_eq!(HostTools.run(&hung), Err(RunError::TimedOut));
    assert_eq!(HostTools.run_complete(&hung, 0), Err(RunError::TimedOut));
    assert_eq!(queue.waited(), std::time::Duration::ZERO);
}

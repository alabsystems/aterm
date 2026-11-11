// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::RunError;
use aterm_uds::deadline::{CompleteOutputError, RunError as CollectorError};

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

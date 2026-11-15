// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::sys::jobs_view::fixtures::row;

#[test]
fn live_verdicts_disclose_missing_progress_observations() {
    for verdict in [
        Verdict::Fine,
        Verdict::Quiet,
        Verdict::Busy,
        Verdict::Slow,
        Verdict::Stuck,
    ] {
        let job = row("a1", 1, Class::Waiting, verdict);
        let wire = job.wire();
        let limits = wire
            .split(" progress_limits=")
            .nth(1)
            .expect("live observation limits");
        assert_eq!(limits.split_whitespace().next(), job.progress_limits());
        let json = crate::sys::jobs_view::row_json(&job, None);
        assert_eq!(
            json.get("progress_limits"),
            Some(&crate::sys::json::Jv::s(job.progress_limits().unwrap()))
        );
        assert_eq!(
            job.said().contains("observation limits:"),
            verdict.is_suspect()
        );

        // Negative control: an observed exit is conclusive, irrespective of the
        // class left on the row; it must not inherit the live progress caveat.
        let mut exited = job;
        exited.verdict = Verdict::Exited;
        exited.exit = Some(0);
        assert!(!exited.wire().contains(" progress_limits="));
        assert!(
            crate::sys::jobs_view::row_json(&exited, None)
                .get("progress_limits")
                .is_none()
        );
        assert!(!exited.said().contains("observation limits:"));
    }
}

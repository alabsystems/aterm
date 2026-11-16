// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// An inert live read: this creates no load, thread, command or GUI request. Its structural
/// assertions establish neither CPU-accounting freshness nor an actual-window CPU share.
#[test]
fn named_self_cpu_live_cut_reads_only_this_generation_and_keeps_counter_brackets() {
    let cut = read(Instant::now()).expect("a self-only Mach read");
    assert_eq!(cut.process.pid, std::process::id());
    assert_ne!(cut.process.start, 0);
    assert!(cut.end_ns >= cut.begin_ns);
    assert!(cut.process_after.ns >= cut.process_before.ns);
    assert_eq!(
        cut.coverage.inspected,
        cut.coverage.listed.min(super::super::MAX_THREADS)
    );
    assert_eq!(
        cut.coverage.capped,
        cut.coverage.listed > super::super::MAX_THREADS
    );
    assert!(cut.coverage.unread <= cut.coverage.inspected);
    assert!(cut.threads.len() <= cut.coverage.inspected);
    assert!(cut.coverage.listed > 0, "the calling test thread is alive");
    assert!(cut.threads.iter().all(|thread| thread.id != 0));
    assert!(
        cut.threads
            .iter()
            .filter_map(|thread| thread.cpu)
            .all(|cpu| cpu.precision_ns == 2000)
    );
}

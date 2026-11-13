// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A stalled reading, as `status` spells it.
const STALLED: &str = "OK schema=1 sid=s-1 hold=0 fabric=connected program=claude \
                       agent=wall:unresponsive integration=- input=stalled input_bytes=1 \
                       input_wait_ms=161000 fg_footprint_mb=39731 supervisor=aterm-harness@1 \
                       seq=9 hash=0\n";

#[test]
fn a_stalled_or_stopped_input_is_a_stall_and_every_other_word_is_not() {
    assert_eq!(
        worker_stall(STALLED),
        Some(WorkerStall {
            word: "stalled",
            bytes: Some(1),
            wait_ms: Some(161_000),
            footprint_mb: Some(39_731),
        })
    );
    let stopped = "OK input=stopped input_bytes=3 input_wait_ms=41000 fg_footprint_mb=-\n";
    let s = worker_stall(stopped).expect("stopped is a stall");
    assert_eq!(
        (s.word, s.bytes, s.footprint_mb),
        ("stopped", Some(3), None)
    );
    for line in [
        "OK input=clear input_bytes=0 input_wait_ms=0 fg_footprint_mb=-\n",
        "OK input=pending input_bytes=1 input_wait_ms=1500 fg_footprint_mb=-\n",
        "OK input=typeahead input_bytes=4 input_wait_ms=30000 fg_footprint_mb=-\n",
        "OK input=- input_bytes=- input_wait_ms=- fg_footprint_mb=-\n",
        "OK schema=1 hold=0 fabric=absent program=claude\n",
        "",
    ] {
        assert_eq!(worker_stall(line), None, "{line}");
    }
    assert!(publishes_input(STALLED));
    assert!(publishes_input("OK input=clear\n"));
    assert!(!publishes_input("OK input=- input_bytes=-\n"));
    assert!(!publishes_input("OK schema=1 hold=0\n"));
}

/// What a stall's warning says after its word: the wait, the backlog
/// and the size, each only where the server measured it.
#[test]
fn the_stall_facts_name_the_wait_the_backlog_and_the_size() {
    let s = worker_stall(STALLED).expect("stall");
    assert_eq!(
        stall_facts(&s),
        " for 2m41s (1 B queued, footprint 38.8 GB)"
    );
    let small = WorkerStall {
        footprint_mb: Some(12),
        wait_ms: None,
        ..s.clone()
    };
    assert_eq!(stall_facts(&small), " (1 B queued, footprint 12 MB)");
    let bare = WorkerStall {
        bytes: None,
        footprint_mb: None,
        wait_ms: Some(41_000),
        ..s
    };
    assert_eq!(stall_facts(&bare), " for 41s");
    assert_eq!(footprint_text(u64::MAX), "footprint 18014398509481984.0 GB");
}

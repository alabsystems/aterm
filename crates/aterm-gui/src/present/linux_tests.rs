// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::{Duration, Instant};

fn progress(now: Instant) -> SubmitProgress {
    SubmitProgress {
        pid: 1,
        birth: None,
        previous: None,
        worked: now,
        sampled: now,
    }
}

fn work(birth: u64, cpu: u64, io: u64) -> PeerReading {
    let mut counters = [0; 9];
    counters[0] = cpu;
    counters[2] = io;
    PeerReading::Work(PeerWork { birth, counters })
}

#[test]
fn only_the_same_display_peers_work_extends_its_confirmation_wait() {
    let began = Instant::now();
    let mut progress = progress(began);
    progress.observe(work(7, 3, 5), began).unwrap();
    progress
        .observe(work(7, 3, 5), began + Duration::from_secs(59))
        .unwrap();
    // Native CPU and I/O progress each start a fresh idle minute.
    progress
        .observe(work(7, 4, 5), began + Duration::from_secs(60))
        .unwrap();
    progress
        .observe(work(7, 4, 6), began + Duration::from_secs(119))
        .unwrap();
    progress
        .observe(work(7, 4, 6), began + Duration::from_secs(178))
        .unwrap();
    assert!(
        progress
            .observe(work(7, 4, 6), began + Duration::from_secs(179))
            .is_err()
    );
}

#[test]
fn unread_or_incomparable_peer_work_cannot_establish_an_idle_minute() {
    let began = Instant::now();
    let mut progress = progress(began);
    progress.observe(work(7, 3, 5), began).unwrap();
    progress
        .observe(PeerReading::Unknown(None), began + Duration::from_secs(60))
        .unwrap();
    progress
        .observe(
            PeerReading::Unknown(Some(7)),
            began + Duration::from_secs(120),
        )
        .unwrap();
    progress
        .observe(work(7, 4, 6), began + Duration::from_secs(180))
        .unwrap();
    progress
        .observe(work(7, 3, 6), began + Duration::from_secs(240))
        .unwrap();
    progress
        .observe(work(7, 3, 6), began + Duration::from_secs(299))
        .unwrap();
    assert!(
        progress
            .observe(work(7, 3, 6), began + Duration::from_secs(300))
            .is_err()
    );
}

#[test]
fn a_reused_or_ended_display_peer_refuses_even_when_its_work_is_unread() {
    let began = Instant::now();
    for reading in [
        PeerReading::Unknown(Some(8)),
        work(8, 4, 6),
        PeerReading::Ended,
    ] {
        let mut progress = progress(began);
        progress.observe(work(7, 3, 5), began).unwrap();
        assert!(
            progress
                .observe(reading, began + Duration::from_secs(1))
                .is_err()
        );
    }
}

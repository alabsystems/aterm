// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

use super::{AgentStamp, SessionTimeline};
use crate::harness_host::STATUS_AFTER_SCREEN;

fn read(tl: &mut SessionTimeline, word: &'static str, seq: u64) {
    tl.publish_agent(
        word,
        None,
        None,
        Some(aterm_phase::Program::Claude),
        AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq },
            fp: seq,
        },
    );
}

#[test]
fn an_idle_screens_move_is_counted_and_wakes_the_host_once_per_settle() {
    let mut tl = SessionTimeline::default();
    read(&mut tl, "idle", 1);
    assert_eq!(
        (tl.agent().rev, tl.agent().screen_moves),
        (1, 0),
        "a verdict's move"
    );
    read(&mut tl, "idle", 2);
    read(&mut tl, "idle", 3);
    assert_eq!(
        (tl.agent().rev, tl.agent().screen_moves),
        (1, 2),
        "/clear: no rev"
    );
    let t0 = Instant::now();
    let mut fresh = SessionTimeline::default();
    read(&mut fresh, "idle", 1);
    assert!(fresh.note_screen_move(t0), "the first move wakes the host");
    assert!(!fresh.note_screen_move(t0 + Duration::from_millis(500)));
    assert!(!fresh.note_screen_move(t0 + STATUS_AFTER_SCREEN - Duration::from_millis(1)));
    assert!(
        fresh.note_screen_move(t0 + STATUS_AFTER_SCREEN),
        "once per settle"
    );
    let mut busy = SessionTimeline::default();
    read(&mut busy, "busy", 1);
    assert!(!busy.note_screen_move(t0), "a busy agent's spinner");
    read(&mut busy, "busy", 2);
    assert_eq!(busy.agent().screen_moves, 0);
    let mut shell = SessionTimeline::default();
    assert!(!shell.note_screen_move(t0), "no agent");
    assert_eq!(shell.agent().screen_moves, 0);
}

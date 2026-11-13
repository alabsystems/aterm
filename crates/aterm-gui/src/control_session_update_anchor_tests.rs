// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{cmd_await, cmd_exits, cmd_timeline};
use crate::session_store::{
    ExitActor, ExitReason, SessionHandoff, SessionStore, new_store, test_handle,
};

#[test]
fn exits_ids_keep_rising_across_a_handoff() {
    let mut producer = SessionStore::default();
    for local in 1..=3 {
        producer.register(test_handle(local));
    }
    producer.deregister_local_as(3, ExitReason::ShellExit, ExitActor::Human);
    assert_eq!(producer.roster_seq(), 4);
    let manifest = SessionHandoff::from_store(&producer);
    let read = SessionHandoff::from_toml(&manifest.to_toml().unwrap()).unwrap();
    assert_eq!(read.roster_seq, Some(4));

    let successor = new_store();
    successor
        .write()
        .unwrap()
        .continue_roster_seq(read.roster_seq.unwrap());
    for rec in &read.sessions {
        successor
            .write()
            .unwrap()
            .register(test_handle(rec.local_id));
    }
    successor
        .write()
        .unwrap()
        .deregister_local_as(1, ExitReason::ShellExit, ExitActor::Human);
    let page = cmd_exits(&successor, "since=4", &[]);
    assert!(page.starts_with("OK 1\n"), "{page}");
    assert!(page.contains("reason=shell-exit"), "{page}");
    let old = SessionHandoff::from_toml(
        "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-old\"\nstate = \"alive\"\ntitle = \"zsh\"\n",
    )
    .expect("an old manifest reads");
    assert_eq!(old.roster_seq, None);
}

#[test]
fn an_adopted_timeline_is_visible_to_a_pre_update_since_anchor() {
    let producer = test_handle(1);
    for i in 0..57 {
        producer
            .ctx
            .timeline
            .lock()
            .unwrap()
            .record_raw_meta_change(format!("n={i}"));
    }
    let producer_timeline = std::sync::Arc::clone(&producer.ctx.timeline);
    let mut store = SessionStore::default();
    store.register(producer);
    let manifest = SessionHandoff::from_store(&store);
    let read = SessionHandoff::from_toml(&manifest.to_toml().unwrap()).unwrap();
    let reserve = crate::session_timeline::HANDOFF_ID_RESERVE;
    assert_eq!(read.sessions[0].timeline_id, 58 + reserve);

    for i in 0..2 {
        producer_timeline
            .lock()
            .unwrap()
            .record_raw_meta_change(format!("overlap={i}"));
    }
    assert!(!producer_timeline.lock().unwrap().past_handoff_ceiling());

    let adopted = test_handle(1);
    crate::spawn::mark_handoff_gap(
        &adopted.ctx.timeline,
        &adopted.ctx.cast,
        &adopted.ctx.temporal,
        Some(7),
        read.sessions[0].timeline_id,
        crate::session_log::LogAdopt {
            ids: read.sessions[0]
                .log_ids
                .as_deref()
                .and_then(crate::session_log::LogCarry::parse),
            seq: read.sessions[0].log_seq,
            lines: None,
            keeper_ep: None,
            mirror: None,
        },
        false,
    );
    adopted
        .ctx
        .timeline
        .lock()
        .unwrap()
        .record_raw_meta_change("after".to_string());
    let page = cmd_timeline(&adopted.ctx, "since=60");
    assert!(page.starts_with("OK 2\n"), "{page}");
    assert!(
        page.contains(&format!("event {} ", 59 + reserve)) && page.contains("kind=handoff"),
        "{page}"
    );
    {
        let mut tl = producer_timeline.lock().unwrap();
        for _ in 0..reserve {
            tl.record_raw_meta_change(String::new());
        }
        assert!(tl.past_handoff_ceiling());
    }
}

#[test]
fn a_seq_anchor_from_another_process_answers_at_once() {
    let handle = test_handle(0);
    let store = new_store();
    handle.term.lock().unwrap().process(b"x");
    let current = handle.term.lock().unwrap().content_seq();
    assert_eq!(
        cmd_await(&handle.term, &store, 0, &handle.ctx, "seq 100000 timeout=0",),
        format!("OK seq {current}\n")
    );
    assert_eq!(
        cmd_await(
            &handle.term,
            &store,
            0,
            &handle.ctx,
            &format!("seq {current} timeout=0"),
        ),
        "OK timeout\n"
    );
}

#[test]
fn await_frame_waits_on_a_half_frame_and_gone_reads_only_whole_ones() {
    const BEFORE: &[u8] =
        include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/before.bin");
    const HALF: &[u8] =
        include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/half.bin");
    const REST: &[u8] =
        include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/rest.bin");
    const END: &[u8] =
        include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/end.bin");
    let handle = test_handle(0);
    let store = new_store();
    let feed = |bytes: &[u8]| handle.term.lock().unwrap().process(bytes);
    let await_ = |rest: &str| cmd_await(&handle.term, &store, 0, &handle.ctx, rest);
    let text = || {
        super::super::control_query::cmd_text_json_read(
            &handle.term,
            &crate::frame_memo::FrameSlot::default(),
            super::super::control_query::TextArgs::default(),
            || (None, None, 0),
            None,
            None,
        )
    };
    const GONE: &str = "gone esc.to.interrupt rows 10 49 timeout=0";
    handle.term.lock().unwrap().resize(50, 144);
    feed(b"\x1b[47;3H");
    feed(BEFORE);
    assert!(await_("frame timeout=0").starts_with("OK frame "));
    assert_eq!(await_(GONE), "OK timeout\n", "busy before the split");
    assert!(!text().contains("\"frame\""), "whole: no frame field");

    feed(HALF);
    assert_eq!(
        await_("frame timeout=0"),
        "OK timeout\n",
        "the frame is open"
    );
    assert_eq!(await_(GONE), "OK timeout\n", "the half frame is not read");
    assert!(text().ends_with(",\"frame\":\"open\"}\n"), "{}", text());

    feed(REST);
    assert!(await_("frame timeout=0").starts_with("OK frame "));
    assert_eq!(await_(GONE), "OK timeout\n", "the whole frame is busy");
    assert!(!text().contains("\"frame\""));

    feed(END);
    assert!(await_(GONE).starts_with("OK gone "), "the reply's end");
}

#[test]
fn await_frame_wakes_at_the_hang_detector_in_the_park() {
    use aterm_core::terminal::{ClockReading, FRAME_HANG};
    use std::time::{Duration, Instant};

    const BEFORE: &[u8] =
        include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/before.bin");
    const HALF: &[u8] =
        include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/half.bin");
    let handle = test_handle(0);
    let store = new_store();
    let text = || {
        super::super::control_query::cmd_text_json_read(
            &handle.term,
            &crate::frame_memo::FrameSlot::default(),
            super::super::control_query::TextArgs::default(),
            || (None, None, 0),
            None,
            None,
        )
    };
    {
        let mut t = handle.term.lock().unwrap();
        t.resize(50, 144);
        t.process(b"\x1b[47;3H");
        t.process(BEFORE);
        let begun = Instant::now()
            .checked_sub(FRAME_HANG - Duration::from_millis(300))
            .expect("an instant a minute back");
        t.process_at(
            HALF,
            ClockReading {
                monotonic: begun,
                wall_ms: None,
            },
        );
        assert!(t.sync_frame_unfinished());
    }
    assert!(text().ends_with(",\"frame\":\"open\"}\n"), "{}", text());
    let started = Instant::now();
    let out = cmd_await(&handle.term, &store, 0, &handle.ctx, "frame timeout=120000");
    assert!(out.starts_with("OK frame "), "{out}");
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "woken by the hang detector, not the timeout"
    );
    assert!(
        !text().contains("\"frame\""),
        "past its hang detector the frame reads as it stands"
    );
    assert!(handle.term.lock().unwrap().sync_frame_unfinished());
}

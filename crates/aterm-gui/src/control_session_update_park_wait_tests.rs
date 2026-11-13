// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::{OnHangup, Park, Parked, Stretch, TurnIo, cmd_await, cmd_ready, cmd_turn};
use crate::session_store::{new_store, test_handle};

#[test]
fn idle_waits_do_not_latch_on_a_parked_readers_silence() {
    let handle = test_handle(0);
    let store = new_store();
    handle.term.lock().unwrap().process(b"tick\r\n");
    handle.ctx.update_parked.park_for_test();
    assert_eq!(
        cmd_await(&handle.term, &store, 0, &handle.ctx, "idle 60 timeout=400",),
        "OK timeout\n"
    );
    assert_eq!(
        cmd_ready(&handle.term, &store, 0, &handle.ctx, "400"),
        "OK timeout\n"
    );
    const IDLE: Duration = Duration::from_millis(200);
    let resumer = {
        let ctx = handle.ctx.clone();
        let term = handle.term.clone();
        std::thread::spawn(move || {
            let first = aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the await arms its idle window",
                || term.lock().unwrap().watch_next_deadline(),
            );
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the await holds its idle window across the park",
                || {
                    let t = term.lock().unwrap();
                    match t.watch_next_deadline() {
                        None => Some(None),
                        Some(at) if at >= first + IDLE && at > Instant::now() => {
                            ctx.update_parked.resume_for_test();
                            Some(Some(at))
                        }
                        Some(_) => None,
                    }
                },
            )
        })
    };
    let reply = cmd_await(
        &handle.term,
        &store,
        0,
        &handle.ctx,
        &format!("idle {} timeout=60000", IDLE.as_millis()),
    );
    let latched = Instant::now();
    let held = resumer.join().unwrap().unwrap_or_else(|| {
        panic!("the await answered {reply:?} while parked, never holding its window open")
    });
    assert!(reply.starts_with("OK idle "), "{reply}");
    assert!(
        latched >= held,
        "a whole window after the last parked look: latched {:?} before it",
        held.saturating_duration_since(latched)
    );
    assert!(
        cmd_await(
            &handle.term,
            &store,
            0,
            &handle.ctx,
            "idle 60 timeout=60000",
        )
        .starts_with("OK idle ")
    );
}

#[test]
fn ready_answers_no_prompt_from_a_parked_readers_parser() {
    let handle = test_handle(0);
    let store = new_store();
    handle.term.lock().unwrap().process(b"\x1b]133;A\x07$ ");
    handle.ctx.update_parked.park_for_test();
    assert_eq!(
        cmd_ready(&handle.term, &store, 0, &handle.ctx, "400"),
        "OK timeout\n",
        "a parked reader's parser says nothing about the shell"
    );
    let resumer = {
        let ctx = handle.ctx.clone();
        let term = handle.term.clone();
        std::thread::spawn(move || {
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the wait arms its settle window",
                || term.lock().unwrap().watch_next_deadline(),
            );
            term.lock()
                .unwrap()
                .process(b"\x1b]133;B\x07make\r\n\x1b]133;C\x07building...");
            ctx.update_parked.resume_for_test();
        })
    };
    let reply = cmd_ready(&handle.term, &store, 0, &handle.ctx, "60000");
    resumer.join().unwrap();
    assert_eq!(
        reply, "OK ready idle\n",
        "the caught-up parser shows `make` running, not the prompt it left"
    );
    handle
        .term
        .lock()
        .unwrap()
        .process(b"\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ");
    assert_eq!(
        cmd_ready(&handle.term, &store, 0, &handle.ctx, "60000"),
        "OK ready prompt\n"
    );
}

#[test]
fn a_turn_across_an_update_park_presses_submit_once() {
    let run = |parked: bool, timeout_ms: u64| -> usize {
        let handle = test_handle(0);
        let store = new_store();
        if parked {
            handle.ctx.update_parked.park_for_test();
        }
        let presses = AtomicUsize::new(0);
        let paste = |_: &str| true;
        let press = |_: &str| {
            presses.fetch_add(1, Ordering::SeqCst);
            true
        };
        let _ = cmd_turn(
            &handle.term,
            &store,
            0,
            &format!("idle=50 timeout={timeout_ms} submit_window=150 presses=3 hello"),
            &handle.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                ..TurnIo::paste_only()
            },
        );
        presses.load(Ordering::SeqCst)
    };
    assert_eq!(run(true, 1200), 1, "one Enter into a parked session");
    assert_eq!(
        run(false, aterm_spec::wait::MUST_HAPPEN.as_millis() as u64),
        3,
        "a swallowed press is re-pressed unparked"
    );
}

#[test]
fn a_park_that_begins_mid_slice_is_noticed_within_one_slice() {
    use std::sync::atomic::AtomicBool;

    static SLICES: AtomicUsize = AtomicUsize::new(0);
    fn slice(left: Duration) -> Duration {
        SLICES.fetch_add(1, Ordering::SeqCst);
        left.min(Duration::from_millis(10))
    }
    fn there() -> bool {
        false
    }
    let lane = crate::park::Lane {
        slice,
        hung_up: there,
    };
    let handle = test_handle(0);
    let sub = crate::session_watch::Subscription::single(&handle.ctx.watchers);
    let alive = || false;
    let mut park = Park::new(
        &sub,
        &alive,
        OnHangup::Abort,
        Stretch::Hold(handle.ctx.update_parked.as_ref()),
    );
    park.lane = lane;

    let entered = AtomicBool::new(false);
    let (seen, at_flip) = std::thread::scope(|scope| {
        let flipper = scope.spawn(|| {
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the park's entry look",
                || entered.load(Ordering::SeqCst).then_some(()),
            );
            handle.ctx.update_parked.park_for_test();
            SLICES.load(Ordering::SeqCst)
        });
        let ended = park.run(
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            |_, parked, _| {
                entered.store(true, Ordering::SeqCst);
                (parked.then(|| SLICES.load(Ordering::SeqCst)), None)
            },
            || {},
        );
        (ended, flipper.join().unwrap())
    });
    let Parked::Done(seen_at) = seen else {
        panic!("the park closed without seeing the flip");
    };
    assert!(
        seen_at <= at_flip + 1,
        "the flip was seen {} slices after it",
        seen_at - at_flip
    );

    handle.ctx.update_parked.resume_for_test();
    let mut looks = 0;
    let ended = park.run(
        Instant::now() + Duration::from_millis(100),
        |_, parked, _| {
            looks += 1;
            (parked.then_some(()), None)
        },
        || {},
    );
    assert!(matches!(ended, Parked::Deadline));
    assert_eq!(
        looks,
        2,
        "entry and close, over {} slices",
        SLICES.load(Ordering::SeqCst)
    );
}

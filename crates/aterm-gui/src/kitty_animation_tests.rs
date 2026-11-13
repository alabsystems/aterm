// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn apc(control: &str, payload: &[u8]) -> Vec<u8> {
    let mut seq = format!("\x1b_G{control}").into_bytes();
    if !payload.is_empty() {
        seq.push(b';');
        seq.extend_from_slice(
            aterm_codec::base64::encode(payload)
                .expect("encode")
                .as_bytes(),
        );
    }
    seq.extend_from_slice(b"\x1b\\");
    seq
}

#[test]
fn a_playing_animation_arms_one_wake_per_frame_and_an_idle_one_none() {
    let mut app = App::headless_for_test();
    let t0 = Instant::now();
    let ms = |n: u64| t0 + Duration::from_millis(n);
    let mut watcher = crate::session_watch::Subscription::new();
    let _ = watcher.watch_frame(&app.pool.get(0).expect("session").ctx.watchers);
    let rang = || watcher.wait(Duration::ZERO);
    app.tick_kitty_animations(0, t0);
    assert_eq!(app.kitty_animation_deadline(), None, "idle: no wake");

    let term = std::sync::Arc::clone(&app.pool.get(0).expect("session").term);
    let placed_frame = || {
        crate::term_lock(&term)
            .images_row(0)
            .first()
            .map(|(_, image)| image.image.bytes[0])
    };
    let at = aterm_core::terminal::ClockReading {
        monotonic: t0,
        wall_ms: None,
    };
    {
        let mut term = crate::term_lock(&term);
        term.set_cell_pixel_size(10, 20);
        for control in [
            "a=t,f=32,s=10,v=20,i=7,q=2",
            "a=f,f=32,s=10,v=20,i=7,z=30,q=2",
        ] {
            let pixel = if control.starts_with("a=t") { 1 } else { 2 };
            term.process_at(&apc(control, &[pixel; 800]), at);
        }
        term.process_at(&apc("a=a,i=7,r=1,z=30", b""), at);
        term.process_at(&apc("a=p,i=7,C=1,q=2", b""), at);
        term.process_at(&apc("a=a,i=7,s=3", b""), at);
    }
    assert!(
        rang(),
        "the placement's hold rang, as the reader's tee would"
    );
    app.tick_kitty_animations(0, ms(10));
    assert_eq!(
        app.kitty_animation_deadline(),
        Some(ms(30)),
        "the root's 30 ms gap, not now"
    );
    assert_eq!(placed_frame(), Some(1));
    assert!(!rang(), "arming the clock shows no frame");

    app.service_kitty_animations(ms(20));
    assert_eq!(placed_frame(), Some(1), "not due yet");
    assert!(!rang(), "no frame shown, no ring");
    app.service_kitty_animations(ms(30));
    assert_eq!(placed_frame(), Some(2), "the due frame is shown");
    assert!(rang(), "the shown frame rang the session's watchers");
    assert_eq!(
        app.kitty_animation_deadline(),
        Some(ms(60)),
        "and the one after it armed"
    );

    crate::term_lock(&term).process_at(&apc("a=a,i=7,s=1", b""), at);
    app.tick_kitty_animations(0, ms(40));
    assert_eq!(app.kitty_animation_deadline(), None, "stopped: no wake");
}

#[test]
fn an_animation_in_a_hidden_tab_waits_until_the_tab_shows() {
    let mut app = App::headless_for_test();
    let window = crate::WindowId(0);
    let t0 = Instant::now();
    let ms = |n: u64| t0 + Duration::from_millis(n);
    let term = std::sync::Arc::clone(&app.pool.get(0).expect("session").term);
    let placed_frame = || {
        crate::term_lock(&term)
            .images_row(0)
            .first()
            .map(|(_, image)| image.image.bytes[0])
    };
    let at = aterm_core::terminal::ClockReading {
        monotonic: t0,
        wall_ms: None,
    };
    {
        let mut term = crate::term_lock(&term);
        term.set_cell_pixel_size(10, 20);
        term.process_at(&apc("a=t,f=32,s=10,v=20,i=7,q=2", &[1; 800]), at);
        term.process_at(&apc("a=f,f=32,s=10,v=20,i=7,z=30,q=2", &[2; 800]), at);
        term.process_at(&apc("a=a,i=7,r=1,z=30", b""), at);
        term.process_at(&apc("a=p,i=7,C=1,q=2", b""), at);
        term.process_at(&apc("a=a,i=7,s=3", b""), at);
    }
    app.push_stub_tab(window, crate::stub_session(1));
    app.tick_kitty_animations(0, ms(10));
    assert_eq!(app.kitty_animation_deadline(), None, "parked: no wake");
    app.service_kitty_animations(ms(100));
    assert_eq!(
        placed_frame(),
        Some(1),
        "a hidden animation holds its frame"
    );
    assert_eq!(app.kitty_animation_deadline(), None, "and stays parked");

    app.switch_tab_in(window, 0);
    app.service_kitty_animations(ms(110));
    assert_eq!(placed_frame(), Some(2), "shown again, it plays on");
    assert_eq!(app.kitty_animation_deadline(), Some(ms(140)));
}

#[test]
fn the_clock_wakes_for_the_soonest_frame_and_never_for_a_parked_one() {
    let t0 = Instant::now();
    let ms = |n| t0 + Duration::from_millis(n);
    let mut clocks = KittyAnimationClocks::default();
    assert_eq!(clocks.deadline(), None, "nothing playing: no wake");
    clocks.next.insert(1, Some(ms(40)));
    clocks.next.insert(2, Some(ms(15)));
    clocks.next.insert(3, None);
    assert_eq!(clocks.deadline(), Some(ms(15)));
    assert_eq!(clocks.due(ms(10)), [3], "only the parked one before 15 ms");
    assert_eq!(clocks.due(ms(15)), [2, 3]);
    assert_eq!(clocks.due(ms(40)), [1, 2, 3]);
    clocks.next.remove(&2);
    clocks.next.remove(&1);
    assert_eq!(clocks.deadline(), None, "a parked animation arms no wake");
}

#[test]
fn tick_kitty_animations_flushes_host_op_without_duplicating_and_defers_off_ground() {
    use crate::temporal::{InputSlice, SpineMark, SpinePre, TemporalMsg, queue_on};
    use aterm_core::terminal::{ClockReading, HostOp};

    let mut app = App::headless_for_test();
    let t0 = Instant::now();
    let ms = |n: u64| t0 + Duration::from_millis(n);
    let (term, spine) = {
        let s = app.pool.get(0).expect("session");
        let spine = s.ctx.temporal.lock().expect("recorder").spine_watermark();
        (std::sync::Arc::clone(&s.term), spine)
    };
    let (tx, rx) = queue_on(spine.clone(), 64);
    let mut reader_mark = SpineMark::attach(spine);
    app.pool.set_temporal_tx_for_test(0, Some(tx.clone()));

    let mut setup = Vec::new();
    setup.extend_from_slice(&apc("a=t,f=32,s=10,v=20,i=7,q=2", &[1; 800]));
    setup.extend_from_slice(&apc("a=f,f=32,s=10,v=20,i=7,z=30,q=2", &[2; 800]));
    setup.extend_from_slice(&apc("a=a,i=7,r=1,z=30", b""));
    setup.extend_from_slice(&apc("a=p,i=7,C=1,q=2", b""));
    setup.extend_from_slice(&apc("a=a,i=7,s=3", b""));
    let at0 = ClockReading {
        monotonic: t0,
        wall_ms: None,
    };
    {
        let mut t = crate::term_lock(&term);
        t.set_cell_pixel_size(10, 20);
        let pre = SpinePre::take(&t);
        t.process_at(&setup, at0);
        reader_mark.record(&tx, pre, &t, InputSlice::at(&setup, at0));
    }
    while rx.try_recv().is_some() {}

    // Leave parser mid-APC at 28ms: tick at 30ms must defer by LOCK_RETRY without advancing!
    {
        let mut t = crate::term_lock(&term);
        let pre = SpinePre::take(&t);
        let partial = b"\x1b_Ga=a,i=7,s=0";
        let at28 = ClockReading {
            monotonic: ms(28),
            wall_ms: None,
        };
        t.process_at(partial, at28);
        reader_mark.record(&tx, pre, &t, InputSlice::at(partial, at28));
        assert!(!t.parser_is_ground());
    }
    while rx.try_recv().is_some() {}

    app.tick_kitty_animations(0, ms(30));
    assert_eq!(
        app.kitty_animation_deadline(),
        Some(ms(30) + LOCK_RETRY),
        "non-ground parser defers wake by LOCK_RETRY"
    );
    assert!(rx.try_recv().is_none(), "no HostOp while mid-sequence");

    // Finish the sequence to Ground, then service at 34ms: advances once and flushes HostOp.
    {
        let mut t = crate::term_lock(&term);
        let pre = SpinePre::take(&t);
        let finish = b"\x1b\\";
        let at32 = ClockReading {
            monotonic: ms(32),
            wall_ms: None,
        };
        t.process_at(finish, at32);
        reader_mark.record(&tx, pre, &t, InputSlice::at(finish, at32));
        assert!(t.parser_is_ground());
    }
    while rx.try_recv().is_some() {}

    app.service_kitty_animations(ms(34));
    let mut flushed = Vec::new();
    while let Some(msg) = rx.try_recv() {
        flushed.push(msg);
    }
    assert_eq!(flushed.len(), 1);
    assert!(matches!(
        flushed[0],
        TemporalMsg::Host(HostOp::AdvanceKittyAnimations, Some(c)) if c.monotonic == ms(34)
    ));

    // Next reader slice on `reader_mark` sees the shared host watermark and sends NO duplicate HostOp.
    {
        let mut t = crate::term_lock(&term);
        let pre = SpinePre::take(&t);
        let more = b"after";
        let at36 = ClockReading {
            monotonic: ms(36),
            wall_ms: None,
        };
        t.process_at(more, at36);
        reader_mark.record(&tx, pre, &t, InputSlice::at(more, at36));
    }
    let mut next_msgs = Vec::new();
    while let Some(msg) = rx.try_recv() {
        next_msgs.push(msg);
    }
    assert!(
        !next_msgs
            .iter()
            .any(|m| matches!(m, TemporalMsg::Host(HostOp::AdvanceKittyAnimations, _))),
        "reader SpineMark must not re-send already-flushed HostOp"
    );
}

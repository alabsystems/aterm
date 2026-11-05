// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's hold on typed keys that wait for their program's echo (ATERM-KI-1025-1): nothing
//! reaches the boundary while a key is held, a release logs its keys at the release instant, and
//! every other way out of a hold stops the recording and taints the glow.

use aterm_time::Instant;

use super::test_fixtures::{LANE, Seen, TAU, attached, cfg, draw, geom, hosted, ms};
use super::{
    KeyEcho, MomentumCode, MomentumEvent, MomentumHost, MomentumSchema, MomentumState,
    PAST_HORIZON, RAW_KEY,
};

/// The host's log since its ancestor checkpoint.
fn log(h: &MomentumHost) -> Vec<MomentumEvent> {
    hosted(h)
        .boundary
        .log()
        .expect("fixture: attached")
        .inputs()
        .to_vec()
}

/// The `Key` inputs' instants in the host's log, in nanoseconds after `epoch`.
fn key_times(h: &MomentumHost) -> Vec<i128> {
    log(h)
        .iter()
        .filter_map(|e| match *e {
            MomentumEvent::Key { at, .. } => Some(at),
            _ => None,
        })
        .collect()
}

/// The typing-rate stamp of the host's attach checkpoint: `None` when the glow started from rest.
fn checkpoint_rate_at(h: &MomentumHost, epoch: Instant) -> Option<i128> {
    let b = &hosted(h).boundary;
    let state: MomentumState = b
        .checkpoint()
        .expect("attached")
        .restore(&MomentumSchema::new(epoch), b.identity())
        .expect("restores");
    state.glow().snapshot(epoch).expect("a snapshot").rate_at
}

/// A frame of the host at `now` with the caret at (4, 10).
fn frame(h: &mut MomentumHost, now: Instant) {
    let _ = draw(h, Some((4, 10)), now, geom(), &cfg(true), 1);
}

#[test]
fn a_hold_appends_nothing_and_a_release_appends_keys_at_the_release_instant() {
    let t0 = Instant::now();
    for hold in [true, false] {
        let mut host = attached(t0, MomentumCode::Shipped);
        host.on_key(t0 + ms(10), TAU);
        frame(&mut host, t0 + ms(12));
        let before = log(&host);
        if hold {
            host.aos_hold(t0 + ms(1_020));
        } else {
            host.on_key(t0 + ms(20), TAU);
            host.on_key(t0 + ms(40), TAU);
        }
        frame(&mut host, t0 + ms(25));
        let _ = host.next_change_deadline(t0 + ms(30), LANE);
        frame(&mut host, t0 + ms(45));
        let _ = host.value(t0 + ms(50), TAU);
        if !hold {
            assert!(
                log(&host).len() >= before.len() + 4,
                "negative control: unheld keys, ticks and queries are logged as they come"
            );
            continue;
        }
        assert_eq!(
            log(&host),
            before,
            "nothing reached the boundary while held"
        );
        assert!(
            !host.is_active() && host.settling(),
            "the lane wakes for the hold"
        );
        host.aos_release(t0 + ms(60), 2, TAU, None);
        assert!(!host.aos_holding());
        assert_eq!(
            key_times(&host),
            [10_000_000, 60_000_000, 60_000_000],
            "both held keys at the release instant"
        );
        assert_eq!(
            log(&host).len(),
            before.len() + 2,
            "the two keys and nothing else"
        );
        frame(&mut host, t0 + ms(70));
        assert!(
            log(&host).len() > before.len() + 2,
            "frames log again after the release"
        );
    }
}

#[test]
fn a_held_glow_holds_still_on_the_caret_the_echo_moves() {
    let t0 = Instant::now();
    // Six keys 30 ms apart and a frame at the caret they were typed at, (4, 10).
    let typed = |host: &mut MomentumHost| {
        for i in 0..6 {
            host.on_key(t0 + ms(10 + 30 * i), TAU);
        }
        frame(host, t0 + ms(162));
    };
    let mut held = attached(t0, MomentumCode::Shipped);
    typed(&mut held);
    held.aos_hold(t0 + ms(1_160));
    let before = log(&held);
    // The echo moves the caret, and later frames find it further on.
    for (cur, now) in [
        ((4, 10), t0 + ms(170)),
        ((4, 11), t0 + ms(180)),
        ((4, 12), t0 + ms(900)),
    ] {
        let mut never_held = MomentumHost::default();
        typed(&mut never_held);
        let still = draw(
            &mut never_held,
            Some(cur),
            t0 + ms(162),
            geom(),
            &cfg(true),
            1,
        );
        let got = draw(&mut held, Some(cur), now, geom(), &cfg(true), 1);
        assert!(
            matches!(&got, Seen::Frame { frame, out, .. } if frame.hot && !out.is_empty()),
            "the glow is lit at {cur:?}"
        );
        assert_eq!(
            got, still,
            "at {cur:?}: the glow as it stood at its last tick, drawn on the caret"
        );
        let flowing = draw(&mut never_held, Some(cur), now, geom(), &cfg(true), 1);
        if now > t0 + ms(500) {
            assert_ne!(got, flowing, "held still, not cooling with the clock");
        }
    }
    assert_eq!(
        log(&held),
        before,
        "nothing reached the boundary while held"
    );
}

#[test]
fn a_partial_release_holds_again_until_the_rest_s_deadline() {
    let t0 = Instant::now();
    let mut host = attached(t0, MomentumCode::Shipped);
    host.aos_hold(t0 + ms(1_000));
    host.aos_release(t0 + ms(150), 1, TAU, Some(t0 + ms(1_050)));
    assert!(host.aos_holding(), "one key still waits");
    let n = log(&host).len();
    frame(&mut host, t0 + ms(160));
    assert_eq!(log(&host).len(), n, "still nothing while the rest waits");
    assert_eq!(
        host.next_change_deadline(t0 + ms(160), LANE),
        Some(t0 + ms(1_050)),
        "the rest's own deadline, not the first key's"
    );
    host.aos_release(t0 + ms(200), 1, TAU, None);
    assert_eq!(key_times(&host), [150_000_000, 200_000_000]);
}

#[test]
fn on_key_or_reset_during_a_hold_detaches_and_taints() {
    let t0 = Instant::now();
    for (what, stray) in [
        (
            "a key",
            (|h, at| h.on_key(at, TAU)) as fn(&mut MomentumHost, Instant),
        ),
        ("a reset", |h, _| h.reset()),
    ] {
        let mut host = attached(t0, MomentumCode::Shipped);
        host.on_key(t0 + ms(10), TAU);
        host.aos_hold(t0 + ms(1_020));
        let rate = host.value(t0 + ms(20), TAU);
        stray(&mut host, t0 + ms(20));
        assert!(
            !host.aos_attached(),
            "{what} while held stopped the recording"
        );
        assert!(
            host.aos_trace().is_err(),
            "{what}: nothing is left to log into"
        );
        assert_eq!(host.aos_take_detached(), Some(RAW_KEY), "{what}");
        assert!(host.aos_tainted(), "{what}: the glow is tainted");
        assert!(
            host.value(t0 + ms(20), TAU) != rate,
            "{what}: it reached the glow, unrecorded"
        );

        let mut control = attached(t0, MomentumCode::Shipped);
        control.on_key(t0 + ms(10), TAU);
        stray(&mut control, t0 + ms(20));
        assert!(
            control.aos_attached(),
            "negative control: {what} with no key held"
        );
        assert!(!control.aos_tainted());
    }
}

#[test]
fn a_dropped_hold_taints_and_the_next_attach_starts_from_rest() {
    let t0 = Instant::now();
    let horizon = crate::cursor_momentum::MOMENTUM_PROFILE.horizon;
    for (what, held) in [("a detach", true), ("a detach, no hold", false)] {
        let mut host = attached(t0, MomentumCode::Shipped);
        host.on_key(t0 + ms(10), TAU);
        if held {
            host.aos_hold(t0 + ms(1_020));
        }
        host.aos_detach();
        assert_eq!(host.aos_tainted(), held, "{what}");
        let epoch = t0 + ms(30);
        host.aos_attach(epoch, MomentumCode::Shipped, super::test_fixtures::BUILD, 2);
        assert!(!host.aos_tainted(), "{what}: the attach took the taint");
        if held {
            assert_eq!(checkpoint_rate_at(&host, epoch), None, "{what}: from rest");
        } else {
            assert_eq!(
                checkpoint_rate_at(&host, epoch),
                Some(-20_000_000),
                "negative control: {what} carries the logged key's momentum"
            );
        }
    }

    let mut host = attached(t0, MomentumCode::Shipped);
    host.on_key(t0 + ms(10), TAU);
    host.aos_hold(t0 + ms(1_020));
    let epoch = t0 + ms(40);
    host.aos_attach(epoch, MomentumCode::Shipped, super::test_fixtures::BUILD, 2);
    assert!(host.aos_attached() && !host.aos_holding() && !host.aos_tainted());
    assert_eq!(
        checkpoint_rate_at(&host, epoch),
        None,
        "a new attach over a hold"
    );

    let mut host = attached(t0, MomentumCode::Shipped);
    host.on_key(t0 + horizon - ms(500), TAU);
    host.aos_hold(t0 + horizon + ms(500));
    frame(&mut host, t0 + horizon + ms(10));
    assert!(!host.aos_attached());
    assert_eq!(host.aos_take_detached(), Some(PAST_HORIZON));
    assert!(host.aos_tainted(), "the horizon dropped the hold");
}

#[test]
fn aos_pause_refuses_while_a_key_is_held() {
    let t0 = Instant::now();
    let mut host = attached(t0, MomentumCode::Shipped);
    host.aos_hold(t0 + ms(1_000));
    let n = log(&host).len();
    assert_eq!(
        host.aos_pause(t0 + ms(10), LANE),
        Err(aterm_aos::publish::PublishRefusal::MidStep)
    );
    assert!(!host.aos_paused() && host.aos_holding());
    assert_eq!(log(&host).len(), n, "the refusal recorded nothing");
    host.aos_release(t0 + ms(20), 1, TAU, None);
    host.aos_pause(t0 + ms(30), LANE)
        .expect("negative control: pauses once released");
    host.aos_hold(t0 + ms(1_000));
    assert!(!host.aos_holding(), "a paused glow holds nothing");
}

#[test]
fn a_hold_names_its_deadline_unlogged_and_never_at_or_before_now() {
    let t0 = Instant::now();
    let mut host = attached(t0, MomentumCode::Shipped);
    host.on_key(t0 + ms(5), TAU);
    let _ = host.next_change_deadline(t0 + ms(6), LANE);
    let (n, query) = (log(&host).len(), hosted(&host).last_query);
    let deadline = t0 + ms(1_005);
    host.aos_hold(deadline);
    assert_eq!(host.next_change_deadline(t0 + ms(10), LANE), Some(deadline));
    let late = t0 + ms(1_500);
    assert_eq!(
        host.next_change_deadline(late, LANE),
        Some(late + LANE),
        "past the deadline: a frame on, never now"
    );
    assert_eq!(log(&host).len(), n, "no query was recorded");
    assert_eq!(hosted(&host).last_query, query);
}

#[test]
fn aos_status_is_unchanged_by_a_hold() {
    let t0 = Instant::now();
    let mut host = attached(t0, MomentumCode::Shipped);
    host.on_key(t0 + ms(5), TAU);
    frame(&mut host, t0 + ms(6));
    let before = host.aos_status();
    host.aos_hold(t0 + ms(1_005));
    frame(&mut host, t0 + ms(20));
    assert_eq!(host.aos_status(), before, "no field shows a held key");
    assert!(
        host.settling() && !host.is_active(),
        "while the lane's own reading does"
    );
}

#[test]
fn a_release_with_no_hold_reaches_only_the_glow() {
    let t0 = Instant::now();
    let mut host = attached(t0, MomentumCode::Shipped);
    host.aos_release(t0 + ms(10), 2, TAU, None);
    assert!(
        !host.aos_attached(),
        "an unheld release stopped the recording"
    );
    assert_eq!(host.aos_take_detached(), Some(RAW_KEY));
    assert!(host.aos_tainted());
    assert!(
        host.value(t0 + ms(10), TAU) > 0.0,
        "the keys reached the glow"
    );

    let mut direct = MomentumHost::default();
    direct.aos_hold(t0 + ms(1_000));
    assert!(!direct.aos_holding(), "an unattached host holds nothing");
    direct.aos_release(t0 + ms(10), 1, TAU, None);
    assert!(direct.aos_tainted() && direct.value(t0 + ms(10), TAU) > 0.0);
}

#[test]
fn a_hold_released_unseen_logs_its_keys_unseen_at_that_instant_and_taints() {
    let t0 = Instant::now();
    for unseen in [true, false] {
        let mut host = attached(t0, MomentumCode::Shipped);
        host.aos_hold(t0 + ms(1_010));
        let release = t0 + ms(400);
        if unseen {
            host.aos_release_unseen(release, 2, TAU);
        } else {
            host.aos_release(release, 2, TAU, None);
        }
        assert!(
            host.aos_attached() && !host.aos_holding(),
            "unseen={unseen}"
        );
        let keys: Vec<_> = log(&host)
            .iter()
            .filter_map(|e| match *e {
                MomentumEvent::Key { at, echo, .. } => Some((at, echo)),
                _ => None,
            })
            .collect();
        let echo = if unseen {
            KeyEcho::Unseen
        } else {
            KeyEcho::Seen
        };
        assert_eq!(
            keys,
            [(400_000_000, echo); 2],
            "unseen={unseen}: both keys at the release, never their presses"
        );
        assert_eq!(
            hosted(&host).state().unseen(),
            if unseen { 2 } else { 0 },
            "unseen={unseen}: the state counts them"
        );
        assert_eq!(
            host.aos_tainted(),
            unseen,
            "unseen={unseen}: only an unseen release taints (negative control: a proven one)"
        );
        let epoch = t0 + ms(500);
        host.aos_attach(epoch, MomentumCode::Shipped, super::test_fixtures::BUILD, 2);
        assert_eq!(
            hosted(&host).state().unseen(),
            0,
            "unseen={unseen}: a new recording starts its count at zero"
        );
        assert_eq!(
            checkpoint_rate_at(&host, epoch).is_none(),
            unseen,
            "unseen={unseen}: the next attach starts from rest only after unseen keys"
        );
    }

    let mut host = attached(t0, MomentumCode::Shipped);
    host.aos_release_unseen(t0 + ms(10), 1, TAU);
    assert!(
        !host.aos_attached(),
        "with no hold open it stops the recording"
    );
    assert_eq!(host.aos_take_detached(), Some(RAW_KEY));
    assert!(host.aos_tainted());
}

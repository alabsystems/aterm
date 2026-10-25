// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A glow in its vivid light-page ink is never recorded, so it draws as an unattached glow does.

use aterm_aos::publish::{PublishRefusal, RETIRED_CAP, Retire};
use aterm_aos::registry::Judged;
use aterm_time::Instant;

use super::test_fixtures::{BUILD, LANE, Seen, TAU, cfg, draw, geom, ms};
use super::{MOMENTUM_QUEUE_CAP, MomentumCode, MomentumHost, VIVID_INK};

/// Types `keys` keys 60 ms apart from `start`, then draws `frames` frames a lane apart, setting the ink to `vivid` before each frame as the cursor effect does; returns every frame and the instant after the last.
fn drive(
    h: &mut MomentumHost,
    vivid: bool,
    start: Instant,
    keys: u16,
    frames: u32,
) -> (Vec<Seen>, Instant) {
    let light = cfg(false);
    let mut seen = Vec::new();
    let mut now = start;
    for k in 0..keys {
        now = start + ms(60 * u64::from(k));
        h.on_key(now, TAU);
        h.set_light_vivid(vivid);
        seen.push(draw(h, Some((4, 10 + k)), now, geom(), &light, 1));
    }
    for _ in 0..frames {
        now += LANE;
        h.set_light_vivid(vivid);
        seen.push(draw(h, Some((4, 10 + keys)), now, geom(), &light, 1));
    }
    (seen, now + ms(1))
}

/// What [`side_by_side`] saw after the repair.
struct After {
    /// The unattached host's frames.
    plain: Vec<Seen>,
    /// The attached host's frames.
    subject: Vec<Seen>,
    /// The attached host's pause answer.
    paused: Result<(), PublishRefusal>,
    /// The attached host's repair answer.
    repaired: Result<(), PublishRefusal>,
    /// The attached host.
    host: MomentumHost,
}

/// Runs an unattached host and one attached under Shipped side by side with the ink `vivid`: a burst, a pause, a repair to Shipped, then more typing and frames.
fn side_by_side(vivid: bool) -> After {
    let t0 = Instant::now();
    let mut plain = MomentumHost::default();
    let mut subject = MomentumHost::default();
    plain.set_light_vivid(vivid);
    subject.set_light_vivid(vivid);
    subject.aos_attach(t0, MomentumCode::Shipped, BUILD, 1);
    let (before_plain, at) = drive(&mut plain, vivid, t0, 12, 0);
    let (before_subject, _) = drive(&mut subject, vivid, t0, 12, 0);
    assert_eq!(
        before_subject, before_plain,
        "vivid={vivid}: before the repair"
    );
    let paused = subject.aos_pause(at, LANE).map(|_| ());
    let repaired = subject.aos_repair(MomentumCode::Shipped).map(|_| ());
    let (after_plain, _) = drive(&mut plain, vivid, at + ms(60), 6, 12);
    let (after_subject, _) = drive(&mut subject, vivid, at + ms(60), 6, 12);
    After {
        plain: after_plain,
        subject: after_subject,
        paused,
        repaired,
        host: subject,
    }
}

#[test]
fn a_glow_in_its_vivid_ink_draws_as_an_unattached_one_through_an_attach_a_pause_and_a_repair() {
    let After {
        plain,
        subject,
        paused,
        repaired,
        host,
    } = side_by_side(true);
    assert_eq!(plain.len(), 18);
    for (k, (s, p)) in subject.iter().zip(&plain).enumerate() {
        assert_eq!(
            s, p,
            "frame {k} after the repair draws as an unattached glow's"
        );
    }
    assert!(!host.aos_attached(), "the vivid glow was never attached");
    assert!(host.light_vivid());
    assert_eq!(paused, Err(PublishRefusal::NotAttached));
    assert_eq!(repaired, Err(PublishRefusal::NotAttached));
}

#[test]
fn control_with_the_walk_toward_white_ink_the_repaired_glow_also_draws_as_an_unattached_one() {
    let After {
        plain,
        subject,
        paused,
        repaired,
        host,
    } = side_by_side(false);
    assert_eq!(subject, plain, "hosting is exact in what it draws");
    assert!(host.aos_attached());
    assert_eq!((paused, repaired), (Ok(()), Ok(())));
}

#[test]
fn turning_the_vivid_ink_on_while_attached_draws_as_an_unattached_glow_from_that_frame() {
    let t0 = Instant::now();
    let mut plain = MomentumHost::default();
    let mut subject = MomentumHost::default();
    subject.attach_with(t0, MomentumCode::Shipped, BUILD, 1, MOMENTUM_QUEUE_CAP, 8);
    let (before_plain, at) = drive(&mut plain, false, t0, 8, 0);
    let (before_subject, _) = drive(&mut subject, false, t0, 8, 0);
    assert_eq!(before_subject, before_plain);
    assert!(subject.aos_attached(), "fixture: recording");
    let waiting = subject.aos_retired_waiting();
    assert!(waiting > 0, "fixture: logs of 8 inputs retired some");
    assert_eq!(subject.aos_take_detached(), None);
    let (after_plain, _) = drive(&mut plain, true, at, 6, 12);
    let (after_subject, _) = drive(&mut subject, true, at, 6, 12);
    for (k, (s, p)) in after_subject.iter().zip(&after_plain).enumerate() {
        assert_eq!(s, p, "frame {k} in the vivid ink");
    }
    assert!(
        !subject.aos_attached(),
        "the vivid ink stopped the recording"
    );
    assert_eq!(subject.aos_take_detached(), Some(VIVID_INK));
    assert_eq!(subject.aos_take_detached(), None, "taken once");
    subject.aos_attach(at + ms(500), MomentumCode::Shipped, BUILD, 2);
    assert!(
        !subject.aos_attached(),
        "an attach in the vivid ink is refused"
    );

    let left = subject.aos_take_retired();
    assert!(
        (waiting..=RETIRED_CAP).contains(&left.len()),
        "the segments retired before the detach are kept: {} of at least {waiting}",
        left.len()
    );
    for seg in left {
        assert_eq!(seg.cause(), Retire::Full);
        assert_eq!(seg.judge().judged, Judged::Pass);
    }
    assert_eq!(subject.aos_retired_waiting(), 0);

    subject.set_light_vivid(false);
    subject.aos_attach(at + ms(600), MomentumCode::Shipped, BUILD, 3);
    assert!(
        subject.aos_attached(),
        "back in the walk-toward-white ink it attaches"
    );
}

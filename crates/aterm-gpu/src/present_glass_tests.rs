// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    ArmedPresent, GlassReport, GlassRing, GlassSample, PresentTag, TAG_RING, arm_deck_target,
    classify, deck_late,
};
use std::sync::Mutex;

#[test]
fn classify_skips_unpresented_frames_and_clamps_the_span() {
    assert_eq!(classify(12.5, 0.0), GlassSample::Skipped);
    assert_eq!(classify(12.5, f64::NAN), GlassSample::Skipped);
    assert_eq!(classify(12.5, -1.0), GlassSample::Skipped);
    let GlassSample::OnGlass { ns } = classify(100.0, 100.016) else {
        panic!("a nonzero presentedTime is on glass");
    };
    assert!(
        (15_999_000..=16_001_000).contains(&ns),
        "16 ms span read as {ns} ns"
    );
    assert_eq!(classify(100.0, 99.0), GlassSample::OnGlass { ns: 0 });
    assert_eq!(
        classify(0.0, 1.0e30),
        GlassSample::OnGlass { ns: u64::MAX },
        "spans exceeding Duration::MAX saturate to u64::MAX instead of panicking"
    );
    assert_eq!(
        classify(-1.0e308, 1.0e308),
        GlassSample::OnGlass { ns: 0 },
        "infinite spans fail closed to 0"
    );
    assert_eq!(
        classify(0.0, f64::MAX),
        GlassSample::OnGlass { ns: u64::MAX },
        "a span past Duration's range saturates rather than panics"
    );
}

/// A present armed with a deck frame's vsync carries it from its registration (the next
/// registration carries none unless armed again, another thread's takes none of it, and a
/// slot a later registration overwrote reads as none), and its report says how late on glass
/// it came: 0 on time, nothing for a frame never shown or a present with no deck frame.
#[test]
fn a_deck_frame_reports_its_lateness_against_its_vsync() {
    static SEEN: Mutex<Vec<GlassReport>> = Mutex::new(Vec::new());
    fn sink(report: GlassReport) {
        SEEN.lock().unwrap().push(report);
    }
    let ring = GlassRing::listening(sink);
    arm_deck_target(Some(300_000_000_000));
    let other = std::thread::scope(|scope| {
        scope
            .spawn(|| ring.registration_begin(299.98).0)
            .join()
            .expect("another thread's present registers")
    });
    let (deck, deck_tag) = ring.registration_begin(299.99);
    let (plain, _) = ring.registration_begin(300.01);
    let _ = ring.take_registration();
    assert_eq!(
        ring.deck_target(other),
        0,
        "another thread's present takes no arm of this one's"
    );
    assert_eq!(ring.deck_target(deck), 300_000_000_000);
    assert_eq!(ring.deck_target(plain), 0, "the arm is the one present's");
    let shown = GlassSample::OnGlass { ns: 1 };
    assert_eq!(
        deck_late(ring.deck_target(deck), shown, 300.004),
        Some(4_000_000)
    );
    assert_eq!(
        deck_late(ring.deck_target(deck), shown, 299.999),
        Some(0),
        "early is on time"
    );
    assert_eq!(
        deck_late(ring.deck_target(deck), GlassSample::Skipped, 300.004),
        None
    );
    assert_eq!(deck_late(ring.deck_target(plain), shown, 300.004), None);
    ring.deliver_presented(deck, deck_tag, 299.99, 300.004);
    assert_eq!(
        SEEN.lock().unwrap().last().map(|r| r.deck_late_ns),
        Some(Some(4_000_000)),
        "the report carries its deck frame's lateness"
    );
    // Each overwriting registration arms a target of its own, so the slot `deck` held keeps
    // a nonzero target: only the serial's tag tells it is another registration's.
    let mut over = None;
    for _ in 0..TAG_RING {
        arm_deck_target(Some(301_000_000_000));
        let serial = ring.registration_begin(301.0).0;
        if serial % TAG_RING as u64 == deck % TAG_RING as u64 {
            over = Some(serial);
        }
    }
    let over = over.expect("CONTROL: a later registration took the slot `deck` held");
    assert_eq!(
        ring.deck_target(over),
        301_000_000_000,
        "CONTROL: the slot holds the overwriting registration's target"
    );
    assert_eq!(
        ring.deck_target(deck),
        0,
        "a slot a later registration overwrote is not this registration's target"
    );
}

#[test]
fn occlusion_wins_then_burst_splits_on_the_bypass() {
    assert_eq!(
        PresentTag::for_pacing(true, true, true),
        PresentTag::Occluded
    );
    assert_eq!(
        PresentTag::for_pacing(false, false, true),
        PresentTag::Occluded
    );
    assert_eq!(
        PresentTag::for_pacing(true, true, false),
        PresentTag::HotBurst
    );
    assert_eq!(
        PresentTag::for_pacing(true, false, false),
        PresentTag::IdleBurst
    );
    assert_eq!(
        PresentTag::for_pacing(false, true, false),
        PresentTag::Spaced
    );
    assert_eq!(
        PresentTag::for_pacing(false, false, false),
        PresentTag::Spaced
    );
}

#[test]
fn a_skip_is_attributed_to_its_successor_unless_occluded() {
    let report = |tag, successor| GlassReport {
        sample: GlassSample::Skipped,
        tag,
        successor,
        deck_late_ns: None,
    };
    assert_eq!(
        report(PresentTag::Spaced, PresentTag::HotBurst).skip_attribution(),
        PresentTag::HotBurst
    );
    assert_eq!(
        report(PresentTag::HotBurst, PresentTag::IdleBurst).skip_attribution(),
        PresentTag::IdleBurst
    );
    assert_eq!(
        report(PresentTag::Occluded, PresentTag::HotBurst).skip_attribution(),
        PresentTag::Occluded
    );
    assert_eq!(
        report(PresentTag::HotBurst, PresentTag::Spaced).skip_attribution(),
        PresentTag::Spaced
    );
    assert_eq!(
        report(PresentTag::Spaced, PresentTag::Occluded).skip_attribution(),
        PresentTag::Spaced
    );
}

#[test]
fn the_tag_is_decided_at_registration_against_the_previous_registration() {
    let ring = GlassRing::new();
    let hz60_ns = 1_000_000_000_000 / 60_000;
    ring.arm_present(ArmedPresent {
        input_hot: true,
        occluded: false,
        interval_ns: hz60_ns,
        prev_registered_ns: None,
    });
    let (first, tag) = ring.registration_begin(100.0);
    assert_eq!(
        tag,
        PresentTag::Spaced,
        "no previous registration is not a burst"
    );
    let reg = ring.take_registration().expect("registration is takeable");
    assert_eq!(reg.serial, first);
    assert_eq!(reg.registered_ns, 100_000_000_000);
    assert_eq!(ring.take_registration(), None, "taken once");

    ring.arm_present(ArmedPresent {
        input_hot: true,
        occluded: false,
        interval_ns: hz60_ns,
        prev_registered_ns: Some(reg.registered_ns),
    });
    let (_, tag) = ring.registration_begin(100.008_33);
    assert_eq!(tag, PresentTag::HotBurst);

    ring.arm_present(ArmedPresent {
        input_hot: false,
        occluded: false,
        interval_ns: hz60_ns,
        prev_registered_ns: Some(reg.registered_ns),
    });
    let (_, tag) = ring.registration_begin(100.016_67);
    assert_eq!(tag, PresentTag::Spaced);

    ring.arm_present(ArmedPresent {
        input_hot: false,
        occluded: false,
        interval_ns: hz60_ns,
        prev_registered_ns: Some(reg.registered_ns),
    });
    let (_, tag) = ring.registration_begin(100.004);
    assert_eq!(tag, PresentTag::IdleBurst);
}

#[test]
fn a_report_names_the_next_registration_as_its_successor() {
    static SEEN: Mutex<Vec<GlassReport>> = Mutex::new(Vec::new());
    fn sink(report: GlassReport) {
        SEEN.lock().unwrap().push(report);
    }
    let ring = GlassRing::listening(sink);
    let spaced = ArmedPresent {
        input_hot: false,
        occluded: false,
        interval_ns: 16_666_666,
        prev_registered_ns: None,
    };
    ring.arm_present(spaced);
    let (a, tag_a) = ring.registration_begin(200.0);
    ring.arm_present(ArmedPresent {
        input_hot: true,
        prev_registered_ns: Some(200_000_000_000),
        ..spaced
    });
    let (b, tag_b) = ring.registration_begin(200.005);
    assert_eq!(tag_b, PresentTag::HotBurst);
    ring.arm_present(spaced);
    let (c, tag_c) = ring.registration_begin(201.0);
    assert_eq!(tag_c, PresentTag::Spaced);

    ring.deliver_report(a, tag_a, GlassSample::Skipped);
    ring.deliver_report(c, tag_c, GlassSample::OnGlass { ns: 1 });
    ring.deliver_report(b, tag_b, GlassSample::OnGlass { ns: 1 });
    assert_eq!(ring.delivered(), 3);
    {
        let seen = SEEN.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].successor, PresentTag::HotBurst, "a → b");
        assert_eq!(seen[0].skip_attribution(), PresentTag::HotBurst);
        assert_eq!(seen[1].successor, tag_c, "c has nothing after it");
    }

    ring.arm_present(spaced);
    let (d, tag_d) = ring.registration_begin(202.0);
    assert_eq!(tag_d, PresentTag::Spaced);
    for i in 0..=u16::try_from(TAG_RING).unwrap() {
        ring.arm_present(ArmedPresent {
            input_hot: true,
            prev_registered_ns: Some(202_000_000_000),
            ..spaced
        });
        let (_, later_tag) = ring.registration_begin(202.001 + f64::from(i) * 0.000_1);
        assert_eq!(later_tag, PresentTag::HotBurst);
    }
    ring.deliver_report(d, tag_d, GlassSample::Skipped);
    let seen = SEEN.lock().unwrap();
    assert_eq!(
        seen[3].successor, tag_d,
        "an overwritten TAG_RING slot falls back to d's own tag instead of impersonating d + 1"
    );
    assert_eq!(seen[3].skip_attribution(), PresentTag::Spaced);
}

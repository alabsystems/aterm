// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The rim ramps in, pulses, climbs as the freeze goes on, thickens and
/// cools toward the electric tint, and is open-ended up to its cap.
#[test]
fn the_charging_rim_ramps_pulses_climbs_and_is_capped() {
    let now = Instant::now();
    let l = LevelUp::charging(831, now, 1.0);
    assert!(l.border_alpha(now) < l.border_alpha(now + CHARGE_RAMP_IN));
    let a = l.border_alpha(now + Duration::from_millis(600));
    let b = l.border_alpha(now + Duration::from_millis(600) + CHARGE_PULSE_PERIOD / 2);
    assert_ne!(a, b, "the rim pulses over a half-period");
    let early = l.border_alpha(now + CHARGE_PULSE_PERIOD / 4 + CHARGE_PULSE_PERIOD);
    let late = l.border_alpha(now + CHARGE_PULSE_PERIOD / 4 + CHARGE_PULSE_PERIOD * 12);
    assert!(late >= early, "charged: {late} vs {early}");
    for ms in (0..4000).step_by(37) {
        let t = now + Duration::from_millis(ms);
        assert!(l.border_alpha(t) <= CHARGE_PEAK_BORDER as u8 + 1);
        assert!(l.wash_alpha(t) <= CHARGE_PEAK_WASH as u8 + 1, "readable");
        assert!(l.border_scale_q4(t) >= CHARGE_THIN_Q4 as u8 - 1);
    }
    assert_ne!(
        l.accent(0x0000_00ff, now + Duration::from_secs(1)),
        0x0000_00ff
    );
    assert!(!l.is_expired(now + Duration::from_secs(60)));
    assert!(l.is_expired(now + CHARGE_TTL));
    assert_eq!(l.border_alpha(now + CHARGE_TTL), 0);
    assert_eq!(l.deadline(now), now + FRAME, "animating every frame");
}

/// Under reduced motion the rim holds still (no pulse, hue travel or extra
/// thickening) and its deadline is its cap.
#[test]
fn reduced_motion_holds_a_still_rim_and_the_successor_continues_it_charged() {
    let now = Instant::now();
    let c = LevelUp::charging(1, now, 0.0);
    let held = now + Duration::from_secs(2);
    assert_eq!(c.border_alpha(held), STILL_BORDER as u8);
    assert_eq!(
        c.border_alpha(held + CHARGE_PULSE_PERIOD / 2),
        STILL_BORDER as u8
    );
    assert_eq!(c.wash_alpha(held), STILL_WASH as u8);
    assert_eq!(c.border_scale_q4(held), STILL_THICK_Q4 as u8);
    assert_eq!(
        c.accent(0x0000_00ff, held),
        c.accent(0x0000_00ff, held + FRAME * 7)
    );
    assert_eq!(
        c.fingerprint(held),
        c.fingerprint(held + Duration::from_secs(20))
    );
    assert_eq!(c.deadline(held), now + CHARGE_TTL);
    assert_eq!(c.deadline(now), now + FRAME, "the ramp-in still animates");

    let c = LevelUp::charging_continued(3, now, 1.0);
    assert!(
        c.border_alpha(now) >= CHARGE_FLOOR_BORDER as u8,
        "no dip to nothing at the swap: {}",
        c.border_alpha(now)
    );
    assert!(c.border_alpha(now) > LevelUp::charging(3, now, 1.0).border_alpha(now));
    assert!(
        (c.pulse(now) - 0.5).abs() < 1e-3,
        "the pulse starts at its midpoint: {}",
        c.pulse(now)
    );
    assert_eq!(c.charge_climb(now), 1.0, "the base has climbed");
    assert!(!c.is_expired(now + Duration::from_secs(60)));
    assert!(c.is_expired(now + CHARGE_TTL));
    let cs = LevelUp::charging_continued(3, now, 0.0);
    assert_eq!(cs.border_alpha(now), STILL_BORDER as u8);
    assert_eq!(cs.deadline(now), cs.deadline(now + Duration::from_secs(5)));
}

/// NEGATIVE CONTROL: `Some(l.fingerprint(now))` is disjoint from the `None` no-rim key,
/// constant within a [`FRAME`] quantum, and distinct across steps and builds.
#[test]
fn fingerprint_is_nonzero_stable_per_frame_and_steps() {
    let now = Instant::now();
    let l = LevelUp::charging(830, now, 1.0);
    assert_ne!(Some(l.fingerprint(now)), None, "never the no-rim sentinel");
    assert_eq!(
        l.fingerprint(now),
        LevelUpRepaintKey {
            build: 830,
            frame_step: Some(0),
        }
    );
    assert_eq!(
        l.fingerprint(now),
        l.fingerprint(now + Duration::from_millis(10))
    );
    assert_eq!(
        l.fingerprint(now + FRAME + Duration::from_millis(1)),
        LevelUpRepaintKey {
            build: 830,
            frame_step: Some(1),
        }
    );
    assert_ne!(
        LevelUp::charging(831, now, 1.0).fingerprint(now),
        l.fingerprint(now)
    );
}

#[test]
fn mix_rgb_interpolates_per_channel_and_clamps() {
    assert_eq!(mix_rgb(0x0000_0000, 0x00ff_ffff, 0.0), 0x0000_0000);
    assert_eq!(mix_rgb(0x0000_0000, 0x00ff_ffff, 1.0), 0x00ff_ffff);
    assert_eq!(mix_rgb(0x0000_0000, 0x00ff_ffff, 0.5), 0x0080_8080);
    assert_eq!(mix_rgb(0x0010_2030, 0x0010_2030, 0.7), 0x0010_2030);
    assert_eq!(
        mix_rgb(0x0000_0000, 0x00ff_ffff, 7.0),
        0x00ff_ffff,
        "clamped"
    );
}

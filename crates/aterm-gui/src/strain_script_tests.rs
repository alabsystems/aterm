// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_vitals::Strain;

fn calm() -> Snapshot {
    let mut snap = Snapshot::default();
    let cpu = &mut snap.resources[Resource::Cpu.index()];
    cpu.capacity = Some(18.0);
    cpu.now = Some(3.0);
    cpu.ring_k = 400;
    cpu.ring = vec![aterm_vitals::ring::Cell::default(); 12].into();
    let gpu = &mut snap.resources[Resource::Gpu.index()];
    gpu.capacity = Some(1.0);
    gpu.now = Some(0.4);
    snap
}

#[test]
fn a_script_parses_its_steps_and_refuses_what_it_cannot_hold() {
    let s = Script::parse("0.1:2, 0.9/0.5:1.5,1.6:2").expect("parses");
    assert_eq!(s.steps.len(), 3);
    assert_eq!(
        s.steps[1],
        Step {
            level: 0.9,
            soak: 0.5,
            hold: Duration::from_secs_f32(1.5)
        }
    );
    for bad in [
        "",
        "0.5",
        "x:2",
        "2.5:1",
        "0.5/1.5:1",
        "0.5:0",
        "0.5:-1",
        "nan:1",
    ] {
        assert!(Script::parse(bad).is_err(), "{bad:?} must not parse");
    }
}

#[test]
fn the_script_cycles_through_its_steps() {
    let s = Script::parse("0.1:2,0.9:1").expect("parses");
    let level = |ms: u64| s.at(Duration::from_millis(ms)).level;
    assert_eq!(level(0), 0.1);
    assert_eq!(level(1999), 0.1);
    assert_eq!(level(2000), 0.9);
    assert_eq!(level(2999), 0.9);
    assert_eq!(level(3000), 0.1);
    assert_eq!(level(5500), 0.9);
}

#[test]
fn a_step_makes_the_strain_read_what_it_says() {
    for (level, soak) in [(0.0, 0.0), (0.35, 0.0), (1.0, 0.2), (1.6, 0.5), (2.0, 0.95)] {
        let step = Step {
            level,
            soak,
            hold: Duration::from_secs(1),
        };
        let strain = Strain::of(&step.apply(&calm()));
        assert!(
            (strain.level.expect("read") - level.min(1.0)).abs() < 1e-6,
            "{level}: {strain:?}"
        );
        let over = (level - 1.0).max(0.0);
        let want_over = if over > 0.0 {
            over.clamp(aterm_vitals::strain::OVER_FLOOR, 1.0)
        } else {
            0.0
        };
        assert!(
            (strain.over - want_over).abs() < 1e-4,
            "{level}: {strain:?}"
        );
        // The soak lands within one bucket's step of the asked value.
        assert!((strain.soak - soak).abs() < 0.02, "{soak}: {strain:?}");
    }
}

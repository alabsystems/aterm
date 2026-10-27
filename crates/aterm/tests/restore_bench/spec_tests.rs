// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The restore measurement's grammar ([`crate::spec`]), checked.

use std::path::PathBuf;

use crate::spec::*;

#[test]
fn an_arm_needs_every_observed_application_and_no_refusals() {
    let copy = Applied {
        copies: 2,
        plants: 0,
        refused: 0,
    };
    let planted = Applied { plants: 2, ..copy };
    assert!(Applied::default().matches(Arm::Clean, 2));
    assert!(copy.matches(Arm::Copy, 2));
    assert!(planted.matches(Arm::Lookalike, 2));
    assert!(!copy.matches(Arm::Clean, 2));
    assert!(!copy.matches(Arm::Lookalike, 2));
    assert!(!planted.matches(Arm::Copy, 2));
    assert!(
        !Applied {
            refused: 1,
            ..planted
        }
        .matches(Arm::Lookalike, 2)
    );
    assert!(
        !Applied {
            copies: 1,
            ..planted
        }
        .matches(Arm::Lookalike, 2)
    );
    assert!(
        !Applied {
            plants: 1,
            ..planted
        }
        .matches(Arm::Lookalike, 2)
    );
    assert!(!Applied::default().matches(Arm::Clean, 0));
}

#[test]
fn the_defaults_are_the_lane_s_and_every_knob_reads_back() {
    let spec = parse("out=/s").expect("parses");
    assert_eq!(
        spec,
        Spec {
            out: PathBuf::from("/s"),
            sessions: vec![1, 8, 32],
            rounds: 3,
            lines: 5_000,
            arm: Arm::Clean,
            lookalike_kib: LOOKALIKE_KIB_DEFAULT,
        }
    );
    assert_eq!(
        spec.words(),
        "sessions=1,8,32 rounds=3 lines=5000 arm=clean"
    );
    let spec = parse("out=/s sessions=2,4 rounds=1 lines=10 arm=lookalike lookalike_kib=64")
        .expect("parses");
    assert_eq!(
        spec.words(),
        "sessions=2,4 rounds=1 lines=10 arm=lookalike lookalike_kib=64"
    );
    assert_eq!(spec.arm.seam(spec.lookalike_kib).as_deref(), Some("64"));
    assert_eq!(Arm::Copy.seam(64).as_deref(), Some("0"));
    assert_eq!(Arm::Clean.seam(64), None, "what ships sets no seam");
}

#[test]
fn a_word_out_of_bounds_repeated_or_unknown_is_refused() {
    for bad in [
        "",
        "sessions=1",
        "out=/s sessions=8,1",
        "out=/s sessions=1,1",
        "out=/s sessions=0",
        "out=/s sessions=65",
        "out=/s sessions=1,,8",
        "out=/s rounds=0",
        "out=/s rounds=21",
        "out=/s lines=x",
        "out=/s arm=Clean",
        "out=/s arm=lookalike lookalike_kib=0",
        "out=/s arm=lookalike lookalike_kib=8193",
        "out=/s lookalike_kib=64",
        "out=/s arm=copy lookalike_kib=64",
        "out=/s out=/t",
        "out=/s arm=copy arm=clean",
        "out=/s warmup=3",
        "out=/s sessions",
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
    assert!(
        parse(&format!(
            "out=/s sessions=1,{SESSIONS_MAX} rounds={ROUNDS_MAX} lines={LINES_MAX}"
        ))
        .is_ok()
    );
}

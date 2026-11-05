// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The restore measurement's grammar ([`crate::spec`]), checked.

use std::path::{Path, PathBuf};

use crate::spec::*;

#[test]
fn an_output_path_is_required_separately_from_the_knobs() {
    assert!(parse("", Path::new("")).is_err());
    assert!(parse("out=/retired", Path::new("/typed-output")).is_err());
}

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
    let spec = parse("", Path::new("/s")).expect("parses");
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
    let spec = parse(
        "sessions=2,4 rounds=1 lines=10 arm=lookalike lookalike_kib=64",
        Path::new("/s"),
    )
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
        "sessions=8,1",
        "sessions=1,1",
        "sessions=0",
        "sessions=65",
        "sessions=1,,8",
        "rounds=0",
        "rounds=21",
        "lines=x",
        "arm=Clean",
        "arm=lookalike lookalike_kib=0",
        "arm=lookalike lookalike_kib=8193",
        "lookalike_kib=64",
        "arm=copy lookalike_kib=64",
        "out=/t",
        "arm=copy arm=clean",
        "warmup=3",
        "sessions",
    ] {
        assert!(parse(bad, Path::new("/s")).is_err(), "{bad:?}");
    }
    assert!(
        parse(
            &format!("sessions=1,{SESSIONS_MAX} rounds={ROUNDS_MAX} lines={LINES_MAX}"),
            Path::new("/s")
        )
        .is_ok()
    );
}

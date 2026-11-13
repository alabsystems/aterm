// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn options(pairs: &[(&'static str, &str)]) -> BTreeMap<&'static str, String> {
    pairs.iter().map(|(k, v)| (*k, (*v).to_string())).collect()
}

fn assert_output_path_round_trip(out: PathBuf) {
    let args = Args::default();
    let plain = Path::new("/scratch/plain");
    let control = bench_spec::parse(&args.bench_words(Arm::Clean), plain).unwrap();
    assert_eq!(control.out, plain);
    let decoded = bench_spec::parse(&args.bench_words(Arm::Clean), &out).unwrap();
    assert_eq!(decoded.out, out);
}

#[test]
fn output_path_with_spaces_round_trips() {
    assert_output_path_round_trip(PathBuf::from("/scratch/with spaces"));
}

#[cfg(unix)]
#[test]
fn output_path_with_non_utf8_bytes_round_trips() {
    use std::os::unix::ffi::OsStringExt as _;
    assert_output_path_round_trip(
        std::ffi::OsString::from_vec(b"/scratch/non-utf8-\xff".to_vec()).into(),
    );
}

#[test]
fn ledger_acceptance_requires_observed_application_not_the_requested_arm() {
    let report = |applied: Applied| {
        aterm_json::json!({
            "sizes": [{"sessions": 2, "rounds": [{
                "sessions": 2,
                "experiment": {"copies": applied.copies, "plants": applied.plants,
                               "refused": applied.refused}
            }]}]
        })
    };
    let copy = Applied {
        copies: 2,
        plants: 0,
        refused: 0,
    };
    let planted = Applied { plants: 2, ..copy };
    for (arm, applied) in [
        (Arm::Clean, Applied::default()),
        (Arm::Copy, copy),
        (Arm::Lookalike, planted),
    ] {
        assert!(validate_applied(&report(applied), arm).is_ok());
    }
    for applied in [
        Applied::default(),
        copy,
        Applied {
            plants: 1,
            ..planted
        },
        Applied {
            refused: 1,
            ..planted
        },
    ] {
        assert!(validate_applied(&report(applied), Arm::Lookalike).is_err());
    }
    assert!(validate_applied(&aterm_json::json!({}), Arm::Clean).is_err());
    assert!(validate_applied(&aterm_json::json!({"sizes": []}), Arm::Clean).is_err());
    assert!(
        validate_applied(
            &aterm_json::json!({"sizes": [{"sessions": 2, "rounds": [{"sessions": 2}]}]}),
            Arm::Copy
        )
        .is_err()
    );
}

#[test]
fn the_default_plan_is_every_arm_with_the_bench_s_defaults() {
    let a = Args::from_options(&options(&[("--no-windows", "")])).expect("parses");
    assert_eq!(a.plan(), Arm::ALL);
    assert_eq!(a.bench_words(Arm::Clean), "arm=clean");
    assert_eq!(
        a.command_line(Arm::Lookalike),
        "targo --unverified verify perf restore-e2e --no-windows --arm lookalike"
    );
    assert_eq!(
        a.build_args(),
        [
            "--profile",
            "test",
            "-p",
            "aterm",
            "--bench",
            "restore_bench"
        ]
    );
    let spec = bench_spec::parse(&a.bench_words(Arm::Lookalike), Path::new("/s"))
        .expect("the bench reads the lane's words");
    assert_eq!((spec.sessions, spec.arm), (vec![1, 8, 32], Arm::Lookalike));
}

#[test]
fn every_knob_reaches_the_bench_and_the_entry_s_command_line() {
    let a = Args::from_options(&options(&[
        ("--sessions", "2,16"),
        ("--rounds", "5"),
        ("--lines", "800"),
        ("--arm", "lookalike,clean"),
        ("--lookalike-kib", "64"),
        ("--passes", "2"),
        ("--profile", "opt"),
    ]))
    .expect("parses");
    assert_eq!(
        a.plan(),
        [Arm::Lookalike, Arm::Clean, Arm::Clean, Arm::Lookalike],
        "A B B A"
    );
    assert_eq!(
        a.bench_words(Arm::Lookalike),
        "sessions=2,16 rounds=5 lines=800 arm=lookalike lookalike_kib=64"
    );
    assert_eq!(
        a.bench_words(Arm::Clean),
        "sessions=2,16 rounds=5 lines=800 arm=clean",
        "the plant is the lookalike arm's alone"
    );
    assert_eq!(
        a.command_line(Arm::Lookalike),
        "targo --unverified verify perf restore-e2e --no-windows --profile opt --sessions \
         2,16 --rounds 5 --lines 800 --arm lookalike --lookalike-kib 64"
    );
    assert_eq!(a.build_args()[1], "opt-test");
}

#[test]
fn what_the_bench_would_refuse_is_refused_before_the_build() {
    for pairs in [
        &[("--sessions", "8,1")][..],
        &[("--sessions", "0")],
        &[("--sessions", "65")],
        &[("--rounds", "0")],
        &[("--lines", "x")],
        &[("--arm", "dirty")],
        &[("--arm", "clean,,copy")],
        &[("--lookalike-kib", "64"), ("--arm", "clean")],
        &[("--lookalike-kib", "0")],
        &[("--passes", "0")],
        &[("--profile", "release")],
        &[("--json-out", "/tmp/r.json")],
    ] {
        assert!(Args::from_options(&options(pairs)).is_err(), "{pairs:?}");
    }
    assert!(
        Args::from_options(&options(&[
            ("--json-out", "/tmp/r.json"),
            ("--arm", "copy")
        ]))
        .is_ok(),
        "one arm, one run, one file"
    );
}

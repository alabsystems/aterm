// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn words(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_owned()).collect()
}

#[test]
fn idle_cpu_parser_preserves_zero_settle_and_refuses_invalid_sampling_windows() {
    let args = parse(&words(&["--output", "fixture", "--settle-ms", "0"])).unwrap();
    assert!(args.settle.is_zero());
    assert_eq!(args.duration, Duration::from_secs(65));
    assert_eq!(args.interval, Duration::from_millis(50));
    for tail in [
        vec!["--duration-ms", "0"],
        vec!["--interval-ms", "0"],
        vec!["--duration-ms", "1", "--interval-ms", "2"],
        vec!["--settle-ms", "-1"],
        vec!["--interval-ms", "bad"],
        vec!["--output", "again"],
        vec!["--unknown", "1"],
        vec!["--band", "unknown"],
        vec!["--interval-ms"],
    ] {
        let mut args = words(&["--output", "fixture"]);
        args.extend(words(&tail));
        assert!(parse(&args).is_err(), "{args:?}");
    }
    assert!(parse(&[]).is_err());
    assert!(parse(&words(&["--output", ""])).is_err());
}

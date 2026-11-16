// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn recorded_stat(ticks: &str) -> String {
    // Recorded Linux fields; comm includes both a space and a closing parenthesis.
    format!(
        "4242 (a) b) S 1 4242 4242 34816 4242 4194304 100 0 0 0 1 2 0 0 \
         20 0 1 0 {ticks} 1000000 200 18446744073709551615"
    )
}

#[test]
fn native_linux_birth_is_raw_field_22_not_an_epoch_age() {
    let stat = recorded_stat("12345");
    let native = native_child_from_stat(4242, &stat).expect("recorded native birth");
    assert_eq!(native.pid(), 4242);
    assert_eq!(native.birth(), 12_345);
    // The existing C4/age conversion keeps its distinct epoch-microsecond value.
    assert_eq!(
        linux_start_us(1_763_000_000, native.birth(), 100),
        Some(1_763_000_123_450_000)
    );
    let maximum = native_child_from_stat(4242, &recorded_stat("18446744073709551615"))
        .expect("full native integer precision");
    assert_eq!(maximum.birth(), u64::MAX);
}

#[test]
fn native_linux_birth_refuses_malformed_or_zero_recorded_fields() {
    for stat in ["no parenthesis", "4242 (a) S 1"] {
        assert_eq!(native_child_from_stat(4242, stat), None);
    }
    for ticks in ["0", "-1", "missing", "18446744073709551616"] {
        assert_eq!(
            native_child_from_stat(4242, &recorded_stat(ticks)),
            None,
            "{ticks}"
        );
    }
    for pid in [0, 1, u32::MAX] {
        assert_eq!(native_child_from_stat(pid, &recorded_stat("12345")), None);
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn blocking_preserves_observed_fields_and_refuses_missing_evidence() {
    let original = Reading {
        cores: 8,
        load1: Some(1.0),
        load5: Some(2.0),
        available: Some(8 << 30),
        physical: Some(16 << 30),
        wired: Some(2 << 30),
        ..Reading::default()
    };
    let mut blocked = original.clone();
    assert!(block_observed_memory(&mut blocked));
    let mut expected = original.clone();
    expected.available = Some(0);
    assert_eq!(blocked, expected);
    for mut missing in [
        Reading {
            available: None,
            ..original.clone()
        },
        Reading {
            available: Some(17 << 30),
            ..original.clone()
        },
        Reading {
            load5: None,
            ..original.clone()
        },
        Reading {
            load1: Some(f64::INFINITY),
            ..original.clone()
        },
        Reading {
            wired: None,
            ..original.clone()
        },
    ] {
        let before = missing.clone();
        assert!(!block_observed_memory(&mut missing));
        assert_eq!(missing, before);
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn functional_budget_and_growth_are_explicit_positive_and_checked() {
    for text in [
        None,
        Some("0"),
        Some("-1"),
        Some("unknown"),
        Some("18446744073709551615"),
    ] {
        let mut args = text
            .map(|text| vec!["--budget".into(), text.into()])
            .unwrap_or_default();
        assert!(positive_mib(&mut args, "--budget").is_err());
    }
    let mut args = vec!["--budget".into(), "1024".into()];
    assert_eq!(
        positive_mib(&mut args, "--budget").unwrap(),
        1024 * 1024 * 1024
    );
    assert!(args.is_empty());
    assert_eq!(bound(101, 20).unwrap(), 142);
    assert!(bound(0, 20).is_err());
    assert!(bound(101, 0).is_err());
    assert!(bound(u64::MAX, 1).is_err());
}

#[test]
fn actual_preparation_peak_must_fit_explicit_budget_and_every_controller_bound() {
    let comparisons = [json!({"components":[{"name":"controller","bound_bytes":200}]})];
    let member = json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"footprint_bytes":100,"lifetime_peak_footprint_bytes":150});
    assert!(preparation_fits(&member, 200, &comparisons).is_ok());
    assert!(preparation_fits(&member, 149, &comparisons).is_err());
    assert!(
        preparation_fits(
            &json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"footprint_bytes":100,"lifetime_peak_footprint_bytes":201}),
            300,
            &comparisons
        )
        .is_err()
    );
    // The later current value can exceed the earlier lifetime reading; neither
    // raw counter can be ignored by preparation admission or the frozen bound.
    let current_larger = json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"footprint_bytes":201,"lifetime_peak_footprint_bytes":1});
    assert!(preparation_fits(&current_larger, 200, &comparisons).is_err());
    assert!(preparation_fits(&current_larger, 300, &comparisons).is_err());
    let within = json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"footprint_bytes":150,"lifetime_peak_footprint_bytes":1});
    assert!(preparation_fits(&within, 200, &comparisons).is_ok());
    assert!(preparation_fits(&json!({}), 300, &comparisons).is_err());
    assert!(preparation_fits(&member, 300, &[json!({"components":[]})]).is_err());
}

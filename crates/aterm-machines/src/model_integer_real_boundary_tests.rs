// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::PValue;

#[test]
fn integer_real_comparison_does_not_round_or_accept_small_fractions() {
    for (integer, real) in [
        (0, -0.0),
        (1, 1.0),
        (-7, -7.0),
        (i64::MIN, -9_223_372_036_854_775_808.0),
    ] {
        assert!(PValue::Int(integer).same(&PValue::Real(real)));
        assert!(PValue::Real(real).same(&PValue::Int(integer)));
    }
    assert!(PValue::Int(1).same(&PValue::Bool(true)));
    let distinct: Vec<bool> = [
        (0, f64::EPSILON / 2.0),
        (0, -f64::EPSILON / 2.0),
        (9_007_199_254_740_993, 9_007_199_254_740_992.0),
        (i64::MAX, 9_223_372_036_854_775_808.0),
        (i64::MIN + 1, -9_223_372_036_854_775_808.0),
        (0, f64::NAN),
        (i64::MAX, f64::INFINITY),
    ]
    .into_iter()
    .map(|(integer, real)| {
        !PValue::Int(integer).same(&PValue::Real(real))
            && !PValue::Real(real).same(&PValue::Int(integer))
    })
    .collect();
    assert_eq!(
        distinct, [true; 7],
        "both directions preserve exact integer identity"
    );
}

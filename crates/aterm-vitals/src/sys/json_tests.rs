// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn finite_numbers_preserve_fractional_rates_and_reject_non_json_values() {
    for value in [0.25, -0.25, f64::from_bits(1), f64::MAX, i64::MAX as f64] {
        let number = Jv::number(value).expect("finite number");
        let text = number.to_text();
        assert_eq!(text.parse::<f64>().unwrap(), value, "{text}");
        assert_eq!(
            number,
            Jv::number(value).unwrap(),
            "numeric equality is stable"
        );
        assert!(contract_violations(&Jv::obj([("rate", number)])).is_empty());
    }
    assert_eq!(Jv::number(0.25).unwrap().to_text(), "0.25");
    assert_eq!(Jv::number(0.0), Some(Jv::Int(0)));
    assert_eq!(Jv::number(-0.0), Some(Jv::Int(0)));
    assert_eq!(Jv::number(17.0), Some(Jv::Int(17)));
    assert_eq!(Jv::number(i64::MIN as f64), Some(Jv::Int(i64::MIN)));
    assert_ne!(Jv::number(i64::MAX as f64), Some(Jv::Int(i64::MAX)));
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(Jv::number(value), None, "{value}");
    }
    // Existing integer callers retain their original signed/saturating behavior.
    assert_eq!(Jv::u(u64::MAX), Jv::Int(i64::MAX));
}

#[test]
fn the_four_shapes_write_as_the_design_spells_them() {
    assert_eq!(Jv::Int(9760).to_text(), "9760");
    assert_eq!(
        Jv::at_least_refused(64, 326).to_text(),
        r#"{"v":64,"bound":"at_least","cause":"refused","unread":326}"#
    );
    assert_eq!(
        Jv::at_most(2540, "reaped_no_tier").to_text(),
        r#"{"v":2540,"bound":"at_most","cause":"reaped_no_tier"}"#
    );
    assert_eq!(
        Jv::why("lane_off:ioreport").to_text(),
        r#"{"why":"lane_off:ioreport"}"#
    );
    assert_eq!(Jv::range(1932, 1825).to_text(), r#"{"lo":1825,"hi":1932}"#);
    assert_eq!(Jv::s("a\"b\\c\n\u{1}").to_text(), r#""a\"b\\c\n\u0001""#);
}

#[test]
fn the_contract_walk_catches_every_other_shape() {
    let good = Jv::obj([
        ("cpu_mc", Jv::Int(10)),
        ("runnable", Jv::at_least_refused(1, 2)),
        ("mhz", Jv::range(1, 2)),
        ("gpu", Jv::why("unsupported")),
        ("split_mc", Jv::obj(SPLIT_PARTS.map(|k| (k, Jv::Int(1))))),
    ]);
    assert!(
        contract_violations(&good).is_empty(),
        "{:?}",
        contract_violations(&good)
    );
    let bad = [
        Jv::obj([("x", Jv::obj([("why", Jv::s("zero"))]))]),
        Jv::obj([("x", Jv::obj([("why", Jv::s("refused")), ("v", Jv::Int(1))]))]),
        Jv::obj([("x", Jv::obj([("v", Jv::Int(1)), ("bound", Jv::s("about"))]))]),
        Jv::obj([(
            "x",
            Jv::obj([
                ("v", Jv::Int(1)),
                ("bound", Jv::s("at_least")),
                ("cause", Jv::s("refused")),
            ]),
        )]),
        Jv::obj([("x", Jv::obj([("lo", Jv::Int(3)), ("hi", Jv::Int(2))]))]),
        Jv::obj([(
            "split_mc",
            Jv::obj([("tab", Jv::Int(1)), ("aterm", Jv::Int(2))]),
        )]),
    ];
    for b in bad {
        assert!(!contract_violations(&b).is_empty(), "{}", b.to_text());
    }
}

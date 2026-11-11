// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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

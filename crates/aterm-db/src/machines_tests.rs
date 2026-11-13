// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn legacy_names_and_account_slots_keep_their_meaning() {
    assert_eq!(Name::parse("M100").unwrap().canonical(), "m100");
    assert_eq!(Name::parse("m0").unwrap().canonical(), "m0");
    assert_eq!(Slot::parse("m3.2").unwrap().number, 2);
    assert_eq!(Slot::parse("m27-2").unwrap().canonical(), "m27.2");
    assert_eq!(Slot::parse("m3").unwrap().number, 1);
    for bad in ["m01", "m-1", "m3.2"] {
        assert!(Name::parse(bad).is_err());
    }
}

fn label(number: u32) -> Label {
    Label::plain(number)
}

/// The one label parser reads every spelling the lab's sources use the same way (design §9):
/// `M109` folds to `m109`; four hex digits after a dash are the membership log's clash
/// suffix, one to three digits the legacy number; a dot is a number; `m106-m38` is one
/// unranked preference on m106 and one on m38; and `Name`, `Slot` and stored label cells agree with it.
/// NEGATIVE CONTROL: with the clash arm removed from `Reading::parse` (four hex digits read
/// as a legacy slot, which has at most three), `m40-3fa2` and `m40-1234` are refused, and
/// this fails at its `m40-3fa2` row: `` called `Result::unwrap()` on an `Err` value ``.
#[test]
fn one_parser_reads_every_label_spelling() {
    let clash = |number, clash| Label {
        number,
        clash: Some(clash),
    };
    for (text, reading) in [
        ("m40", Reading::Machine(label(40))),
        (" M109 ", Reading::Machine(label(109))),
        ("m112", Reading::Machine(label(112))),
        ("m40-3fa2", Reading::Machine(clash(40, 0x3fa2))),
        ("M40-3FA2", Reading::Machine(clash(40, 0x3fa2))),
        ("m40-1234", Reading::Machine(clash(40, 0x1234))),
        ("m27-2", Reading::Slot(label(27), 2)),
        ("m27-100", Reading::Slot(label(27), 100)),
        ("m3.2", Reading::Slot(label(3), 2)),
        (
            "m106-m38",
            Reading::Pair {
                first: label(106),
                second: label(38),
            },
        ),
    ] {
        assert_eq!(Reading::parse(text).unwrap(), reading, "{text}");
    }
    for bad in [
        "",
        "m",
        "m01",
        "x1",
        "m1234567",
        "m3.0",
        "m3.02",
        "m3.1000",
        "m27-02",
        "m27-2a",
        "m40-3fa",
        "m40-3fa2b",
        "m106-m106",
        "m106-m38-m2",
        "m3.2.1",
        "mac-mini-m4",
    ] {
        assert!(Reading::parse(bad).is_err(), "{bad:?}");
    }
    assert_eq!(
        Reading::parse("m106-m38").unwrap().preferences(),
        [
            Preferred {
                machine: label(106),
                rank: None,
            },
            Preferred {
                machine: label(38),
                rank: None,
            },
        ]
    );
    assert_eq!(Reading::parse("m3").unwrap().preferences()[0].rank, Some(1));
    assert_eq!(
        Reading::parse("m27-3").unwrap().preferences()[0].rank,
        Some(3)
    );
    assert_eq!(Reading::parse("m27-2").unwrap().machine(), &label(27));
    // Stored cells have one spelling; Name and Slot read through the same grammar.
    assert_eq!(Label::parse("M40-3FA2").unwrap().canonical(), "m40-3fa2");
    assert!(Label::parse_stored("m40-3fa2").is_ok());
    for unstored in ["M40", " m40", "m40-3FA2", "m27-2", "m106-m38"] {
        assert!(Label::parse_stored(unstored).is_err(), "{unstored}");
    }
    assert!(Slot::parse("m40-3fa2").is_err());
    assert!(Slot::parse("m40-1234").is_err());
    assert!(Slot::parse("m106-m38").is_err());
    assert!(Name::parse("m1234567").is_err());
}

/// A machine assignment 0.146 or 0.147 replicated keeps validating: those builds took any
/// machine number that fits, where `Name::parse` now takes six digits; a name no build
/// wrote is still refused.
/// NEGATIVE CONTROL: with `validate` reading the name through `Name::parse`, the
/// seven-digit name is refused: `` called `Result::unwrap()` on an `Err` value ``.
#[test]
fn assignments_older_builds_replicated_keep_validating() {
    let key = "cd".repeat(16);
    validate(&key, &aterm_json::json!({"name": "m1234567"})).unwrap();
    validate(&key, &aterm_json::json!({"name": "M3"})).unwrap();
    validate(&key, &aterm_json::json!({"name": "m0"})).unwrap();
    for bad in ["m01", "m", "x3", "m3-2"] {
        assert!(
            validate(&key, &aterm_json::json!({"name": bad})).is_err(),
            "{bad}"
        );
    }
}

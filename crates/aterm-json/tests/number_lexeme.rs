// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact source-number lookup through a validated JSON document.

use aterm_json::PathSegment::{Index, Key};
use aterm_json::{Value, from_slice, number_lexeme_at_path};

#[test]
fn a_number_lexeme_does_not_round_trip_through_f64() {
    let input = br#"{"usage":[{"used\u005fpercentage":62.999999999999999}]}"#;
    let path = [Key("usage"), Index(0), Key("used_percentage")];

    assert_eq!(
        number_lexeme_at_path(input, &path).expect("valid JSON"),
        Some("62.999999999999999")
    );

    let value: Value = from_slice(input).expect("the same document parses as a Value");
    assert_eq!(
        value
            .get("usage")
            .and_then(|usage| usage.get_index(0))
            .and_then(|row| row.get("used_percentage"))
            .and_then(Value::as_f64),
        Some(63.0),
        "NEGATIVE CONTROL: materialising the number loses its conservative boundary"
    );
}

#[test]
fn decoded_keys_and_last_duplicate_entries_match_value() {
    let input = br#"{"row":{"rate":1,"\u0072ate":2.50,"rate":3e-1}}"#;
    let path = [Key("row"), Key("rate")];
    assert_eq!(
        number_lexeme_at_path(input, &path).expect("valid JSON"),
        Some("3e-1")
    );

    let later_non_number = br#"{"rate":4,"r\u0061te":false}"#;
    assert_eq!(
        number_lexeme_at_path(later_non_number, &[Key("rate")]).expect("valid JSON"),
        None,
        "the later decoded duplicate replaces the earlier number"
    );
    let value: Value = from_slice(later_non_number).expect("Value parses");
    assert_eq!(value.get("rate").and_then(Value::as_bool), Some(false));

    assert_eq!(
        number_lexeme_at_path(br#"{"\uD83D\uDE80":-0}"#, &[Key("🚀")])
            .expect("surrogate pair key is valid"),
        Some("-0")
    );
}

#[test]
fn typed_array_paths_and_misses_return_the_expected_option() {
    let input = br#"[{"n":-0},{"n":"1"},5]"#;
    for (path, expected) in [
        (vec![Index(0), Key("n")], Some("-0")),
        (vec![Index(1), Key("n")], None),
        (vec![Index(2)], Some("5")),
        (vec![Index(3)], None),
        (vec![Key("0")], None),
    ] {
        assert_eq!(
            number_lexeme_at_path(input, &path).expect("valid JSON"),
            expected,
            "path {path:?}"
        );
    }
    assert_eq!(
        number_lexeme_at_path(input, &[]).expect("valid JSON"),
        None,
        "the root is an array, not a number"
    );
    assert_eq!(
        number_lexeme_at_path(b"-12.30e+4", &[]).expect("valid root number"),
        Some("-12.30e+4")
    );
}

#[test]
fn a_match_does_not_short_circuit_validation() {
    let path = [Key("target")];
    for input in [
        br#"{"target":62.9,"later":[0,]}"#.as_slice(),
        br#"{"target":62.9,"later":"\uD800"}"#.as_slice(),
        br#"{"target":62.9} trailing"#.as_slice(),
    ] {
        assert!(
            number_lexeme_at_path(input, &path).is_err(),
            "the entire document must be valid: {}",
            String::from_utf8_lossy(input)
        );
    }
}

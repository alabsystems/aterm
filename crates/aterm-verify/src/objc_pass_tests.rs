// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn wording_changes_keep_stable_case_association() {
    let names = ["first", "second"];
    let before = "first\tthe first window answered\nsecond\tthe second window answered\n";
    let after = "first\tthe first window answered\nsecond\tthe callback waited; Cancel emitted no request\n";
    assert_eq!(
        lookup(before, &names, "first"),
        lookup(after, &names, "first")
    );
    assert_eq!(
        lookup(after, &names, "second"),
        Some("the callback waited; Cancel emitted no request")
    );
    assert_eq!(lookup(after, &names, "unknown"), None);
}

#[test]
fn invalid_metadata_never_returns_a_partial_description() {
    for table in [
        "first\tvalid\n", // missing case
        "first\tvalid\nfirst\tduplicate\n",
        "first\tvalid\nunknown\tunknown case\n",
        "second\tout of order\nfirst\tout of order\n",
        "first\tvalid\nsecond\tvalid\nthird\textra case\n",
        "first\tvalid\nsecond\tvalid\n\n",
        "first\tvalid\nsecond\tvalid", // missing row terminator
        "first\tvalid\nsecond\t\n",
        "first\tvalid\nsecond\t  \n",
        "first\tvalid\nsecond\tvalid\tGateFailed\n", // unknown field
        "first\tvalid\nsecond\tvalid\r\n",
        "first\tvalid\nsecond\t\u{1b}[32mspoofed verdict\n",
        "first\tvalid\nsecond: \"Rust is not the table grammar\"\n",
    ] {
        assert_eq!(
            lookup(table, &["first", "second"], "first"),
            None,
            "{table:?}"
        );
    }
    assert_eq!(
        lookup("first\ta\nfirst\tb\n", &["first", "first"], "first"),
        None
    );
}

#[test]
fn every_registered_driver_has_one_description() {
    for drive in super::super::OBJC_DRIVES {
        assert!(description(drive.example).is_some(), "{}", drive.example);
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn text_fields_round_trip_and_stay_one_word() {
    for text in ["m2", "Andrew’s MacBook Pro", "Mac14,6", "a b=c%d"] {
        let escaped = escape(text);
        assert!(
            !escaped.contains(' ') && !escaped.contains('='),
            "{escaped}"
        );
        assert_eq!(unescape(&escaped).unwrap(), text);
    }
    assert!(unescape("%0A").is_err(), "a control character is refused");
}

/// NEGATIVE CONTROL: `Line::parse` ignoring bare words and keeping duplicate keys lets
/// `"OK state=asking state=allowed"` or `"OK state=asking stray"` pass `answer(...)`.
#[test]
fn line_parse_rejects_bare_words_empty_keys_and_duplicate_keys() {
    let ok = answer("OK state=asking\n").expect("single trailing newline");
    assert_eq!(ok.get("state").unwrap(), "asking");

    for malformed in [
        "",
        " OK state=asking",
        "OK  state=asking",
        "OK state=asking ",
        "OK state=asking stray",
        "OK =1 state=asking",
        "OK state=asking state=allowed",
        "k=v state=asking",
    ] {
        assert!(
            answer(malformed).is_err(),
            "malformed line was accepted: {malformed:?}"
        );
    }

    assert_eq!(
        busy_left("NO busy=1 left=90 why=busy"),
        Some(Duration::from_secs(90))
    );
    assert_eq!(busy_left("OK left=90"), None);
    assert_eq!(busy_left("NO left=90 why=refused"), None);
    assert_eq!(busy_left("NO busy=1 busy=0 left=90 why=busy"), None);
}

/// The lab mark is one `lab=1` field a line parses with, absent for any other identity, and
/// any other value is refused rather than read as unmarked.
#[test]
fn the_lab_field_round_trips_and_takes_only_one() {
    let marked_line = format!("ask node=00{}", lab_field(Some(crate::LabMark::TestBinary)));
    assert_eq!(marked_line, "ask node=00 lab=1");
    assert!(marked(&Line::parse(&marked_line)).unwrap());
    let plain = format!("ask node=00{}", lab_field(None));
    assert_eq!(plain, "ask node=00");
    assert!(!marked(&Line::parse(&plain)).unwrap());
    for other in ["ask lab=0", "ask lab=", "ask lab=yes"] {
        assert!(marked(&Line::parse(other)).is_err(), "{other}");
    }
}

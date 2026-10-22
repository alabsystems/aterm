// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The built-in rules' lines, by their structure: the mark, the closed fact, the rule's name, the
//! caps — never their prose word for word (the red-team's F17).

use super::*;

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;

/// Every line is the mark, one fact sentence within [`FACT_CAP`], constant words, and its rule's
/// name last, all printable ASCII and under [`LINE_CAP`] — across the numbers it can be filled
/// with, the largest included: nothing but numbers and closed words goes in.
#[test]
fn every_line_is_the_mark_a_closed_fact_and_its_rule() {
    let spans = [
        0,
        29 * MIN,
        30 * MIN,
        59 * MIN,
        HOUR,
        5 * HOUR + 6 * MIN,
        i64::MAX,
    ];
    let mut lines = Vec::new();
    for held in spans {
        lines.push(wait_nudge(held));
        for (usable, n) in [(1, 1), (1, 3), (2, 3), (u16::MAX, u16::MAX)] {
            for free in [20 * 60_000, u64::MAX] {
                for avail in [8 * 1024, 96 * 1024, u64::MAX] {
                    lines.push(hold_room(held, usable, n, free, avail));
                }
            }
        }
    }
    for line in &lines {
        let text = line.text();
        assert!(text.starts_with("[aterm harness] aterm: "), "{text}");
        assert!(text.contains(line.fact()), "{text}");
        assert!(
            text.ends_with(&format!(" (rule {})", line.rule().name())),
            "{text}"
        );
        assert!(line.fact().chars().count() <= FACT_CAP, "{text}");
        assert!(text.chars().count() < LINE_CAP, "{text}");
        assert!(
            text.chars().all(|c| c == ' ' || c.is_ascii_graphic()),
            "printable ASCII only: {text}"
        );
    }
}

/// The nudge asks for the declaration; `hold-room` does not (its hold may be declared already),
/// and each says the numbers it was given.
#[test]
fn each_rule_says_its_own_numbers() {
    let nudge = wait_nudge(30 * MIN);
    assert_eq!(nudge.rule(), RuleId::WaitNudge);
    assert!(nudge.fact().contains("30 min"), "{}", nudge.text());
    assert!(nudge.text().contains("WAITING:"), "{}", nudge.text());

    let room = hold_room(5 * HOUR + 6 * MIN, 1, 3, 20 * 60_000, 96 * 1024);
    assert_eq!(room.rule(), RuleId::HoldRoom);
    for said in ["5h06m", "1 of 3", "20 min", "96 GiB"] {
        assert!(room.fact().contains(said), "{said} in {}", room.text());
    }
    assert!(!room.text().contains("WAITING:"), "{}", room.text());
}

/// A span is whole minutes under the hour, hours and two-digit minutes past it, rounded down.
#[test]
fn spans_read_in_minutes_then_hours() {
    for (ms, want) in [
        (0, "0 min"),
        (-5, "0 min"),
        (59_999, "0 min"),
        (29 * MIN + 59_999, "29 min"),
        (59 * MIN, "59 min"),
        (HOUR, "1h00m"),
        (5 * HOUR + 6 * MIN, "5h06m"),
        (26 * HOUR + 59 * MIN, "26h59m"),
    ] {
        assert_eq!(span(ms), want, "{ms}");
    }
}

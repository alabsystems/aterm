// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Keep, SEEN, carry, seen};
use crate::Row;
use aterm_json::Value;

const DAY: u64 = 86_400_000;

fn row(pk: &str, text: &str) -> Row {
    let mut row = Row {
        pk: pk.to_owned(),
        live: true,
        ..Row::default()
    };
    row.cells.insert("note".to_owned(), text.to_owned());
    row.values.insert("note".to_owned(), Value::from(text));
    row
}

fn pks(rows: &[Row]) -> Vec<&str> {
    rows.iter().map(|row| row.pk.as_str()).collect()
}

/// A row the store still holds keeps the time this Mac first saw it say what it says; a changed
/// row is stamped now; a row the store forgot is shown until its span from that time passes,
/// then left out.
///
/// NEGATIVE CONTROL, observed: with the age test removed (every forgotten row carried), the
/// assertion on the 121st day failed with `left: ["a", "b"]`, `right: ["b"]`.
#[test]
fn a_forgotten_row_is_kept_for_its_span_from_when_this_mac_last_saw_it_change() {
    let keep = super::MINE;
    let first = carry(&[], vec![row("a", "one"), row("b", "two")], keep, 0);
    assert_eq!(
        first.iter().map(seen).collect::<Vec<_>>(),
        [Some(0), Some(0)]
    );
    assert_eq!(first[0].cell(SEEN), "0");

    // Day 10: `a` is the same, `b` changed.
    let second = carry(
        &first,
        vec![row("a", "one"), row("b", "three")],
        keep,
        10 * DAY,
    );
    assert_eq!(seen(&second[0]), Some(0), "an unchanged row keeps its time");
    assert_eq!(
        seen(&second[1]),
        Some(10 * DAY),
        "a changed row is seen now"
    );

    // Day 100: the store holds only `b`. `a` is carried, 100 days after it last changed.
    let third = carry(&second, vec![row("b", "three")], keep, 100 * DAY);
    assert_eq!(pks(&third), ["a", "b"]);
    assert_eq!(third[0].cell("note"), "one");
    assert_eq!(seen(&third[0]), Some(0));

    // Day 121: `a`'s 120 days from its last change have passed; `b`'s have not.
    let fourth = carry(&third, vec![], keep, 121 * DAY);
    assert_eq!(pks(&fourth), ["b"]);
}

/// The budget is bytes, never a count: past it the rows this Mac saw change least recently go
/// first, and a projection inside it keeps every row however many there are.
#[test]
fn past_the_budget_the_least_recently_changed_rows_go_first() {
    let rows: Vec<Row> = (0..300).map(|i| row(&format!("r{i:03}"), "x")).collect();
    let each = {
        let mut stamped = rows[0].clone();
        stamped.values.insert(SEEN.to_owned(), Value::from(0_u64));
        stamped.cells.insert(SEEN.to_owned(), "0".to_owned());
        stamped.encoded_len()
    };
    let roomy = Keep {
        age_ms: DAY,
        bytes: each * 300,
    };
    assert_eq!(carry(&[], rows.clone(), roomy, 0).len(), 300);

    let old = carry(&[], rows[..2].to_vec(), roomy, 0);
    let mut read = rows[1..].to_vec();
    read[0].cells.insert("note".to_owned(), "y".to_owned());
    read[0].values.insert("note".to_owned(), Value::from("y"));
    let tight = Keep {
        age_ms: DAY,
        bytes: each * 3,
    };
    // `r000` was seen at 0 and forgotten, `r001` changed at 5 and the rest are new at 5: the
    // budget holds three, so the oldest goes first, then the highest keys of one age.
    let kept = carry(&old, read, tight, 5);
    assert_eq!(kept.len(), 3);
    assert!(!pks(&kept).contains(&"r000"));
}

/// A row that says no time (an earlier projection's) ages from now, and a forgotten one whose
/// time is in this Mac's future is kept.
#[test]
fn a_row_that_says_no_time_ages_from_now() {
    let unstamped = row("a", "one");
    let kept = carry(&[unstamped], vec![], super::LOG, 40 * DAY);
    assert_eq!(seen(&kept[0]), Some(40 * DAY));
    let ahead = carry(&kept, vec![], super::LOG, DAY);
    assert_eq!(pks(&ahead), ["a"]);
}

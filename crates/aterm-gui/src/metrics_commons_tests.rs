// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_db::runtime::SeatCounts;

/// `metrics commons` spells each seat Commons repair counted as its label and its counts
/// ([`emit_seats`], which `emit_commons_seats` hands `aterm_db::runtime::seats`): in text
/// `label:n/n/…`, the counts in the seat's one order and the seats `,`-joined; in JSON an object
/// of seats, each an object of its counts by name in that order; `none` (`{}`) with no seat.
///
/// NEGATIVE CONTROL (2026-11-16, an inverse edit then undone): with each seat emitted as its
/// first count (`wire::Cell::Count`) in place of a record of them, the text read
/// `112233445566:3,…:0`; `control_query.rs::metrics_commons_names_each_seat`, which reads the
/// process's empty ledger, still passed.
#[test]
fn each_commons_seat_is_its_label_and_its_counts_by_name() {
    let seat = |label: &str, counts: [u64; 3]| SeatCounts {
        label: label.to_string(),
        cells: ["out", "rest", "sweep-discarded"]
            .into_iter()
            .zip(counts)
            .collect(),
    };
    let seats = [
        seat("112233445566", [3, 0, 1]),
        seat("112233445566-actor-0001a5a5a5a5", [0, 2, 0]),
    ];
    assert_eq!(
        wire::render_text(|s| emit_seats(&seats, s)),
        " commons_seats=112233445566:3/0/1,112233445566-actor-0001a5a5a5a5:0/2/0"
    );
    assert_eq!(
        wire::render_json(|s| emit_seats(&seats, s)),
        r#"{"commons_seats":{"112233445566":{"out":3,"rest":0,"sweep-discarded":1},"112233445566-actor-0001a5a5a5a5":{"out":0,"rest":2,"sweep-discarded":0}}}"#
    );
    assert_eq!(
        wire::render_text(|s| emit_seats(&[], s)),
        " commons_seats=none"
    );
    assert_eq!(
        wire::render_json(|s| emit_seats(&[], s)),
        r#"{"commons_seats":{}}"#
    );
}

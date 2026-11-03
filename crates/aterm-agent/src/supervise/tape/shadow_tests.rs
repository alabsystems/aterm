// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

use astream_effects::{MAX_SHADOW_ROWS, ShadowDecisionOutcome, ShadowLedger};

use super::{Shadow, ShadowRow, ledgered, point_taint, shadow, shadow_onto};
use crate::supervise::tape::acts::Candidate;
use crate::supervise::tape::doors;

/// An approval at `mark`: decided again as recorded (`Some(false)`), otherwise (`Some(true)`),
/// or not at all (`None`).
fn row(mark: u64, differs: Option<bool>) -> ShadowRow {
    ShadowRow {
        door: doors::DECIDE_APPROVAL.to_string(),
        mark,
        seq: mark,
        recorded_rule: Some("approve@v1".to_string()),
        recorded: "Approve".to_string(),
        candidate_rule: differs.map(|_| "approve@v1".to_string()),
        candidate: differs.map(|d| if d { "Escalate" } else { "Approve" }.to_string()),
        differs,
        taint: false,
        needs: Vec::new(),
        note: differs
            .is_none()
            .then(|| "a look the tape does not hold".to_string()),
    }
}

/// A ledger holding `points` approvals, each decided again; the first otherwise when `first_differs`.
fn ledger_of(points: usize, first_differs: bool) -> ShadowLedger {
    let mut ledger = ShadowLedger::new();
    for k in 0..points {
        let outcome = ShadowDecisionOutcome::Decided {
            candidate: "Approve".to_string(),
            differs: first_differs && k == 0,
        };
        ledger
            .record_point(
                k as u64,
                doors::DECIDE_APPROVAL,
                "Approve",
                outcome,
                Vec::new(),
            )
            .expect("the ledger has room");
    }
    ledger
}

fn taints(rows: &[ShadowRow]) -> Vec<bool> {
    rows.iter().map(|r| r.taint).collect()
}

fn same_differs_same() -> Vec<ShadowRow> {
    vec![row(1, Some(false)), row(2, Some(true)), row(3, Some(false))]
}

/// The taint is set on every row after the first that differs, not on that row itself, and the
/// shadow then exits 1. NEGATIVE CONTROL: reading the ledger's live latch once a row's own point
/// is in (`ShadowLedger::is_tainted`) latches the taint on the differing row itself, which this
/// case refuses.
#[test]
fn a_row_after_the_first_that_differs_is_tainted_and_that_row_is_not() {
    let mut ledger = ShadowLedger::new();
    let (rows, refused) = ledgered(&mut ledger, same_differs_same().into_iter(), point_taint);
    assert_eq!(refused, None);
    assert_eq!(taints(&rows), [false, false, true]);
    let shadow = Shadow {
        tape: "t-1".to_string(),
        recorded_by: None,
        shadowed_by: "this".to_string(),
        candidate: Vec::new(),
        rows,
        unshadowed: None,
        ledger,
    };
    assert_eq!((shadow.differs(), shadow.code()), (1, 1));

    let (latched, _) = ledgered(
        &mut ShadowLedger::new(),
        same_differs_same().into_iter(),
        ShadowLedger::is_tainted,
    );
    assert_ne!(
        taints(&latched),
        [false, false, true],
        "NEGATIVE CONTROL: the taint latched on the differing row itself"
    );
}

/// The ledger holds at most `MAX_SHADOW_ROWS` points: the first row it refuses ends the feed,
/// and that row is the last one made.
#[test]
fn a_full_ledger_ends_the_feed_at_the_row_it_refuses() {
    let made = Cell::new(0);
    let rows = [1, 2, 3].into_iter().map(|mark| {
        made.set(made.get() + 1);
        row(mark, Some(false))
    });
    let mut ledger = ledger_of(MAX_SHADOW_ROWS - 1, false);
    let (taken, refused) = ledgered(&mut ledger, rows, point_taint);
    assert_eq!(taken.iter().map(|r| r.mark).collect::<Vec<_>>(), [1]);
    let refused = refused.expect("the second row refused");
    assert_eq!((refused.mark, refused.left), (2, 2), "{refused:?}");
    assert!(!refused.why.is_empty(), "{refused:?}");
    assert_eq!(made.get(), 2, "no row is made after the one refused");
}

/// A decision the ledger refused is one not decided again: the SHADOW names it, and exits 3 where
/// nothing it holds differs (1 where one does), as a tape longer than the ledger would.
#[test]
fn a_tape_past_the_ledger_names_what_it_refused_and_exits_3() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/testdata/tapes/c1-approval.atape.txt"
    );
    let bytes = std::fs::read(path).expect("c1-approval reads");
    let whole = shadow(&bytes, &Candidate::default()).expect("c1-approval shadows");
    assert_eq!(whole.unshadowed, None);
    let decisions = whole.rows.len();
    assert!(decisions > 1, "{}", whole.render());

    let full = shadow_onto(
        &bytes,
        &Candidate::default(),
        ledger_of(MAX_SHADOW_ROWS, false),
    )
    .expect("c1-approval shadows");
    assert!(full.rows.is_empty(), "{}", full.render());
    let refused = full.unshadowed.as_ref().expect("every decision refused");
    assert_eq!(
        (refused.mark, refused.left),
        (whole.rows[0].mark, decisions)
    );
    assert_eq!(full.code(), 3, "{}", full.render());
    assert_eq!(
        full.ledger.exit_code(),
        0,
        "the aggregate shadow must count the refused points outside the ledger"
    );

    let full_after_one_otherwise = shadow_onto(
        &bytes,
        &Candidate::default(),
        ledger_of(MAX_SHADOW_ROWS, true),
    )
    .expect("c1-approval shadows");
    assert_eq!(full_after_one_otherwise.code(), 1);
}

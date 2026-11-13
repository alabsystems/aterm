// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::task_record::{TaskMark, TaskState};

fn row(local: u64, window: u64, closed_unix_ms: u64) -> Retired {
    let (record, _) = TaskMark::new(TaskState::Done).apply(
        None,
        &Default::default(),
        std::time::Instant::now(),
        closed_unix_ms,
    );
    Retired {
        sid: SessionId::new(format!("s-{local:04x}")),
        local: Some(local),
        window: Some(WindowId(window)),
        title: format!("worker {local}"),
        last_words: None,
        record,
        closed_unix_ms,
    }
}

fn listed(kept: &RetiredWorkers, window: u64) -> Vec<u64> {
    kept.listed_on(WindowId(window))
        .map(|(local, _)| local)
        .collect()
}

/// THE ROWS ARE BOUNDED BY COUNT AND BY AGE ALONE: past [`RETIRED_MAX`] the oldest goes, and a
/// row a day old goes. Dismiss and Clear Finished take rows off their page and drop none, so a
/// manager still reads what its worker left. CONTROL: a row a moment short of a day stays;
/// another window's rows stay listed; a quit drops every row.
#[test]
fn the_rows_are_bounded_by_count_and_age_and_a_page_only_unlists() {
    let mut kept = RetiredWorkers::default();
    for local in 0..u64::try_from(RETIRED_MAX).expect("small") {
        assert!(kept.keep(row(local, 0, 1_000 + local)).is_empty());
    }
    let pushed = kept.keep(row(999, 1, 5_000));
    assert_eq!(pushed.len(), 1);
    assert_eq!(pushed[0].local, Some(0), "the oldest goes first");
    assert!(kept.by_local(0).is_none());
    assert_eq!(
        kept.by_sid(&SessionId::new("s-03e7")).map(|row| row.local),
        Some(Some(999))
    );

    let aged = kept.drop_aged(1_001 + RETIRED_FOR_MS);
    assert_eq!(
        aged.iter().map(|row| row.local).collect::<Vec<_>>(),
        [Some(1)],
        "a day old goes, a moment short of it stays"
    );
    assert!(kept.by_local(2).is_some());

    assert_eq!(kept.unlist_local(2), Some(WindowId(0)), "Dismiss");
    assert_eq!(kept.unlist_local(2), None, "listed nowhere now");
    assert!(kept.by_local(2).is_some(), "Dismiss drops nothing");
    assert!(!listed(&kept, 0).contains(&2));
    assert!(kept.unlist_window(WindowId(1)), "Clear Finished");
    assert!(!kept.unlist_window(WindowId(1)));
    assert!(listed(&kept, 1).is_empty());
    assert!(kept.by_local(999).is_some(), "Clear Finished drops nothing");
    assert_eq!(
        listed(&kept, 0).len(),
        RETIRED_MAX - 3,
        "CONTROL: another window's rows stay listed"
    );

    assert_eq!(kept.drop_all().len(), RETIRED_MAX - 1);
    assert!(listed(&kept, 0).is_empty());
}

/// A SEAMLESS UPDATE CARRIES EVERY ROW: its sid, title, last words, close time and record
/// read back whole, listed on no page and keyed by sid alone. CONTROL: a row that names no
/// session id, a sid already kept, and a record that does not read are skipped.
#[test]
fn a_seamless_update_carries_every_row_unlisted() {
    let now = std::time::Instant::now();
    let mut outgoing = RetiredWorkers::default();
    let mut first = row(0xa1, 3, 7_000);
    first.title = "Claim API credit".to_string();
    first.last_words = Some("Nothing left: CLAIMED: $5".to_string());
    outgoing.keep(first.clone());
    outgoing.keep(row(0xb2, 3, 8_000));
    let rows = outgoing.carry_rows(now);
    assert_eq!(rows.len(), 2);

    let mut successor = RetiredWorkers::default();
    let mut carried = rows.clone();
    carried.push("sid=not-a-sid closed=1 title=x rev=1 state=done".to_string());
    carried.push(rows[1].clone());
    carried.push("sid=s-c3 closed=9 title=x state=done".to_string());
    assert_eq!(successor.adopt(&carried, None, now, 9_000), 2);
    let back = successor
        .by_sid(&SessionId::new("s-00a1"))
        .expect("carried");
    assert_eq!(
        (
            back.local,
            back.window,
            back.title.as_str(),
            back.last_words.as_deref(),
            back.closed_unix_ms,
        ),
        (
            None,
            None,
            "Claim API credit",
            Some("Nothing left: CLAIMED: $5"),
            7_000
        )
    );
    assert_eq!(
        (back.record.state, back.record.rev, &back.record.from),
        (first.record.state, first.record.rev, &first.record.from)
    );
    assert!(successor.by_local(0xa1).is_none(), "keyed by sid alone");
    assert!(
        successor.listed_on(WindowId(3)).next().is_none(),
        "unlisted"
    );
    assert!(successor.by_sid(&SessionId::new("s-c3")).is_none());
}

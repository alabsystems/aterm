// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{InputRefusal, paste_order};

#[test]
fn only_a_full_queue_refuses_a_paste_busy() {
    use crate::input::InputOutcome;
    assert_eq!(
        super::rejected_outcome(paste_order::Rejected::Full),
        InputOutcome::QueueFull
    );
    assert_eq!(
        super::rejected_outcome(paste_order::Rejected::TooLarge),
        InputOutcome::WriteFailed
    );
    assert_eq!(
        super::rejected_outcome(paste_order::Rejected::Unavailable),
        InputOutcome::WriteFailed
    );
}

#[test]
fn each_refusal_maps_to_its_cause_and_a_burst_is_one_row() {
    assert_eq!(
        InputRefusal::of_rejected(paste_order::Rejected::TooLarge),
        InputRefusal::PasteTooLarge
    );
    assert_eq!(
        InputRefusal::of_rejected(paste_order::Rejected::Full),
        InputRefusal::Busy
    );
    assert_eq!(
        InputRefusal::of_rejected(paste_order::Rejected::Unavailable),
        InputRefusal::Unavailable
    );
    let mut app = crate::App::headless_for_test();
    app.clear_messages_for_test();
    let key = "session.input-full.7";
    let row = |app: &crate::App| {
        app.messages
            .live_by_key(key)
            .map(|live| (live.id, live.msg.title.clone(), live.msg.detail.clone()))
    };
    app.refuse_input(7, InputRefusal::Unavailable);
    let (first, _, first_detail) = row(&app).expect("the row");
    for _ in 0..5 {
        app.refuse_input(7, InputRefusal::Unavailable);
    }
    assert_eq!(row(&app).map(|r| r.0), Some(first), "a burst is one row");
    app.refuse_input(7, InputRefusal::Busy);
    let (_, _, detail) = row(&app).expect("still one row");
    assert_ne!(detail, first_detail, "a new cause replaces the line");
    assert_eq!(
        app.messages
            .live_rows()
            .filter(|live| live.msg.key.as_deref() == Some(key))
            .count(),
        1
    );
}

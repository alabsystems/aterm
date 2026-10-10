// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_messages::{Hold, Message, PROGRESS_GRACE, STALE_TAILED, Severity, tags};

use crate::App;

/// The rows a handoff did not carry end with the parent (ruling 441 (d)), the
/// host's half: the park notes the ids its carry handed over, and the Commit's
/// close ends every other live row, and only those. The engine's own test
/// hands `close_uncarried` the carry's ids itself, so a park that noted none
/// (the Commit then closing the carried rows too) would fail nothing there.
#[test]
fn the_park_notes_its_carry_and_the_commit_closes_only_the_rest() {
    let mut parent = App::headless_for_test();
    let shown = parent
        .post_message(Message::new(tags::CONFIG, Severity::Warn, "Disk nearly full").line("why"));
    let unrevealed = parent.post_message(
        Message::new(tags::PACKAGES, Severity::Info, "Checking ALab tools")
            .in_flight()
            .key("toolchain.pass")
            .hold(Hold::Live {
                stale_after: STALE_TAILED,
            })
            .reveal_after(PROGRESS_GRACE),
    );
    let window = parent.park_window_carry(0).expect("a window");
    let carried: Vec<u64> = window.messages.iter().map(|m| m.id).collect();
    assert!(
        carried.contains(&shown.raw()) && !carried.contains(&unrevealed.raw()),
        "premise: the carry takes the warning and leaves the unrevealed row: {carried:?}"
    );
    assert_eq!(
        parent.handoff_carried_messages, carried,
        "the park notes what it handed over"
    );
    assert_eq!(
        parent.close_uncarried_messages(),
        1,
        "the Commit closes the row the carry left, and only it"
    );
    assert!(
        parent.messages.live(shown).is_some(),
        "the carried row is the successor's now: never closed here"
    );
    assert!(parent.messages.live(unrevealed).is_none());
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_a_current_unchanged_restore_may_use_saved_focus() {
    let mut intent = Intent::default();
    let revision = intent.begin();
    assert_eq!(intent.choice(&revision), Choice::Saved);
    intent.note(WindowId(7));
    assert_eq!(intent.choice(&revision), Choice::Window(WindowId(7)));
    intent.finish();
    assert_eq!(intent.choice(&revision), Choice::Stale);
    let next = intent.begin();
    assert_eq!(intent.choice(&next), Choice::Saved);
    assert_eq!(Intent::default().choice(&next), Choice::Stale);
}

#[test]
fn counter_wrap_keeps_the_latest_choice_and_invalidates_an_old_revision() {
    let mut intent = Intent {
        sequence: u64::MAX,
        ..Intent::default()
    };
    let revision = intent.begin();
    intent.note(WindowId(9));
    assert_eq!(intent.choice(&revision), Choice::Window(WindowId(9)));
}

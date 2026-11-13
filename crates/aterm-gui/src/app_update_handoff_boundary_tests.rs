// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{Boundary, GuardedItem, is_held};

#[test]
fn an_item_on_the_process_registry_stays_out_of_a_tests_handoff() {
    let _held = Boundary::process()
        .try_hold(GuardedItem::HarnessNativeMutation)
        .expect("no test latches the process's registry");
    assert!(
        !is_held(GuardedItem::HarnessNativeMutation),
        "another test's handoff would read this guard as activity"
    );
}

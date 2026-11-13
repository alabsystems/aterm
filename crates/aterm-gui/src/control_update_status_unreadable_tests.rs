// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(target_os = "macos")]
#[test]
fn an_unreadable_ledger_says_why_not_no_updater() {
    let line = super::update_status_unreadable_line();
    for field in ["enabled=unknown", "auto_apply=unknown"] {
        assert!(line.split_whitespace().any(|word| word == field), "{line}");
    }
    assert!(
        line.contains(&aterm_update::ledger_unreadable_cause()),
        "{line}"
    );
    assert!(!line.contains("no updater"), "{line}");
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// With no restart in flight the snapshot's words refuse or end a click, and a tab with no
/// agent waits only while a relaunch is due.
#[test]
fn the_snapshots_words_refuse_or_end_a_click() {
    assert_eq!(
        snapshot_word("not-a-shell-job", false),
        Look::Refused(Refusal::NotAShellJob)
    );
    assert_eq!(
        snapshot_word("shell-dialect", false),
        Look::Refused(Refusal::Shell)
    );
    assert_eq!(
        snapshot_word(NO_LANE, false),
        Look::Refused(Refusal::NoLane)
    );
    assert_eq!(
        snapshot_word("argv-unreadable", false),
        Look::Refused(Refusal::ArgvUnread)
    );
    assert_eq!(
        snapshot_word("not-claude", false),
        Look::Ended(End::NotClaude)
    );
    assert_eq!(
        snapshot_word("not-claude", true),
        Look::Waits(Wait::Relaunch)
    );
    assert_eq!(
        snapshot_word("no-foreground", false),
        Look::Waits(Wait::Other("no-foreground".to_string()))
    );
}

/// A `meta` reply's trusted folder, pct-decoded; `-` and a relative path name none.
#[test]
fn a_trusted_folder_is_read_off_meta() {
    assert_eq!(
        trusted_folder(at!("OK cwd=/a trusted_cwd=", "/s/credit%20claims", "")),
        Some(std::path::PathBuf::from(at!("/s/credit claims")))
    );
    assert_eq!(trusted_folder("OK cwd=/a trusted_cwd=-"), None);
    assert_eq!(trusted_folder("OK cwd=/a trusted_cwd=rel"), None);
    assert_eq!(trusted_folder("OK cwd=/a"), None);
}

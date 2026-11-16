// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// With no restart in flight the snapshot's words the host's read answered refuse or end a
/// click, and a tab with no agent waits only while a relaunch is due. LS-15: a word of a fact not
/// read (argv, terminal, image, session record) never refuses or ends it: the click waits for a
/// read that answers. NEGATIVE CONTROL: the click's own classifier refused one for an argv not
/// read, ending it for good on a read a loaded host failed once.
#[test]
fn the_snapshots_words_refuse_or_end_a_click() {
    assert_eq!(
        snapshot_word("not-a-shell-job", false),
        Look::Refused(Refusal::NotAShellJob)
    );
    assert_eq!(
        snapshot_word(NO_LANE, false),
        Look::Refused(Refusal::NoLane)
    );
    // LS-72: arguments another user's process (`sudo claude`) withholds from aterm for as long as
    // it runs are an answer every later read gives again: the click is refused, never left
    // standing for a read that cannot come.
    assert_eq!(
        snapshot_word(relaunch::ARGV_WITHHELD, false),
        Look::Refused(Refusal::ArgvWithheld)
    );
    assert_eq!(
        snapshot_word(relaunch::ARGV_WITHHELD, true),
        Look::Refused(Refusal::ArgvWithheld)
    );
    for unread in [
        relaunch::ARGV_UNREAD,
        "terminal-unread",
        relaunch::EXE_UNREADABLE,
        relaunch::RECORDS_UNREAD,
        "ids",
    ] {
        assert_eq!(
            snapshot_word(unread, false),
            Look::Waits(Wait::Other(unread.to_string())),
            "{unread}: not read"
        );
    }
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

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// The latch re-arms when the strand clears, so a channel that heals and breaks again is
/// announced again.
#[test]
fn the_strand_notice_fires_once_per_strand_and_re_arms_when_it_clears() {
    let mut notice = StrandNotice::default();
    assert!(!notice.due(false), "a readable channel announces nothing");
    assert!(notice.due(true), "the first stranded cycle announces");
    assert!(!notice.due(true), "and only the first");
    assert!(!notice.due(true));
    assert!(!notice.due(false), "the channel reads again");
    assert!(
        notice.due(true),
        "a second strand in the same process is announced again"
    );
    assert!(!notice.due(true));
}

#[test]
fn announce_writes_the_full_explanation_into_the_status_outcome() {
    let _serialized = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let staging = Staging::scratch("unread-status");
    let explanation = "aterm cannot read its release channel github.com/o/r: this \
                       machine stays on its current version until the channel is fixed";
    announce(&staging, 1234, explanation);
    let (_, body) = notification();
    assert!(body.contains("github.com/o/r"), "{body}");
    let text = std::fs::read_to_string(&staging.status).expect("status written");
    assert!(text.contains("stays on its current version"), "{text}");
    let _: aterm_toml::Value = aterm_toml::from_str(&text).expect("status stays valid TOML");
    assert!(is_stranded(), "the latch arms for the background loop");
    clear();
    assert!(
        !is_stranded(),
        "and disarms as soon as the channel reads again"
    );
}

/// THE STRAND IS THE LEDGER'S: whether the channel is unreadable is what the machine's last check
/// recorded in the status ledger, not the latch of the process that made it, so a long-lived
/// window that checked, the node job and a fresh `aterm update status` say the same. A process
/// whose own latch never armed reads the strand; a later readable check by another process clears
/// it for every process, this one's still-armed latch included; and another build's strand is not
/// this build's, as another build's outcome is not.
/// NEGATIVE CONTROL: a status whose `channel_unreadable` is this process's latch fails "a process
/// that did not check reads the strand".
#[test]
fn every_process_reads_the_strand_the_last_check_recorded() {
    use aterm_types::update_standing::UpdateStanding;
    let _serialized = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let staging = Staging::scratch("unread-shared");
    let unreadable = |build| crate::status_at(&staging, build, true).channel_unreadable;
    crate::status::clear_check_note();
    announce(&staging, 1234, "aterm cannot read its release channel");
    clear();
    assert!(
        unreadable(1234),
        "a process that did not check reads the strand"
    );
    assert_eq!(
        crate::status_at(&staging, 1234, true).standing(1234, None),
        UpdateStanding::Unreadable
    );
    assert!(
        !unreadable(1235),
        "another build's strand is not this one's"
    );

    announce(&staging, 1234, "aterm cannot read its release channel");
    crate::status::clear_check_note();
    crate::status::record(&staging, 1234, "up to date");
    assert!(is_stranded(), "this process's latch is still armed");
    assert!(
        !unreadable(1234),
        "a readable check by another process clears it for every one"
    );
    clear();
}

/// THE STRAND MARKS ITS OWN RECORD ALONE: the process that found the channel unreadable goes on
/// writing status records before its next check starts (a refusal, a failed apply, the in-process
/// swap's activation under the newer build), and none of them says the strand. Otherwise the new
/// build, which never checked, reads as unable to check from its first launch, and a refusal of
/// the process that stranded writes the strand back over a readable check another process recorded
/// since.
/// NEGATIVE CONTROL: `announce` setting this process's delivery note (`status::set_delivery_note`)
/// before its record fails "the newer build never checked".
#[test]
fn a_strand_is_said_by_its_own_record_and_no_later_one() {
    let _serialized = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let staging = Staging::scratch("unread-own-record");
    let unreadable = |build| crate::status_at(&staging, build, true).channel_unreadable;
    crate::status::clear_check_note();
    announce(&staging, 1234, "aterm cannot read its release channel");
    assert!(unreadable(1234), "the strand's own record says it");
    // The in-process swap records the newer build's activation, with no check started between.
    crate::status::record(&staging, 1235, "activating build 1235");
    assert!(!unreadable(1235), "the newer build never checked");

    announce(&staging, 1234, "aterm cannot read its release channel");
    std::thread::scope(|scope| {
        // Another process, with delivery facts of its own, reads the channel.
        scope.spawn(|| crate::status::record(&staging, 1234, "up to date"));
    });
    assert!(!unreadable(1234), "a readable check clears the strand");
    crate::status::record(&staging, 1234, "the staged update was refused");
    assert!(
        !unreadable(1234),
        "a later record of the process that stranded does not write it back"
    );
    clear();
}

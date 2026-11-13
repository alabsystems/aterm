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

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::{
    account_progress::AccountProgress,
    footer::{FactsOwner, FooterFacts},
};

fn facts(owner: &str) -> FooterFacts {
    FooterFacts {
        owner: Some(FactsOwner {
            floor: Some(100),
            session_id: owner.into(),
        }),
        ..FooterFacts::default()
    }
}

#[test]
fn refreshed_facts_keep_only_the_same_process_and_conversations_progress() {
    let mut timeline = SessionTimeline::for_test();
    timeline.agent.program_pgid = 42;
    assert!(timeline.set_claude_footer(42, Some(100), Some(facts("one"))));
    let progress = AccountProgress::unknown(101);
    assert!(timeline.set_account_progress(42, Some(100), Some("one"), Some(progress.clone())));
    timeline.set_claude_footer(42, Some(100), Some(facts("one")));
    assert_eq!(
        timeline.claude_footer().unwrap().account_progress,
        Some(progress.clone())
    );
    timeline.set_claude_footer(42, Some(100), Some(facts("two")));
    assert_eq!(timeline.claude_footer().unwrap().account_progress, None);
    assert!(!timeline.set_account_progress(42, Some(100), Some("one"), Some(progress.clone())));
    assert!(timeline.set_account_progress(42, Some(100), Some("two"), Some(progress)));
    timeline.set_claude_footer(42, Some(102), Some(facts("two")));
    assert_eq!(timeline.claude_footer().unwrap().account_progress, None);
}

#[test]
fn recovery_control_snapshot_follows_the_current_foreground_owner() {
    use crate::harness_recovery_verb::snapshot;
    let mut timeline = SessionTimeline::for_test();
    assert_eq!(snapshot(&timeline), None);
    timeline.agent.program_pgid = 42;
    timeline.set_claude_footer(42, Some(100), Some(facts("one")));
    let progress = AccountProgress::unknown(101);
    timeline.set_account_progress(42, Some(100), Some("one"), Some(progress.clone()));
    assert_eq!(snapshot(&timeline), Some(progress.clone()));
    timeline.agent.program_pgid = 43;
    assert_eq!(snapshot(&timeline), None, "old foreground must not leak");
    timeline.agent.program_pgid = 42;
    timeline.set_claude_footer(42, Some(102), Some(facts("two")));
    assert_eq!(
        snapshot(&timeline),
        None,
        "new process has no observation yet"
    );
    assert!(!timeline.set_account_progress(42, Some(100), Some("one"), Some(progress)));
    assert_eq!(snapshot(&timeline), None);
}

#[test]
fn progress_unavailability_is_bound_and_only_a_fresh_read_clears_it() {
    let mut timeline = SessionTimeline::for_test();
    timeline.agent.program_pgid = 42;
    timeline.set_claude_footer(42, Some(100), Some(facts("one")));
    let progress = AccountProgress::unknown(101);
    timeline.set_account_progress(42, Some(100), Some("one"), Some(progress.clone()));
    assert!(timeline.set_account_progress_busy(42, Some(100), Some("one")));
    assert!(!timeline.set_account_progress_busy(42, Some(100), Some("one")));
    timeline.set_claude_footer(42, Some(100), Some(facts("one")));
    assert!(timeline.claude_footer().unwrap().account_progress_busy);
    assert_eq!(
        timeline.claude_footer().unwrap().account_progress,
        Some(progress.clone())
    );
    for (pid, birth, owner) in [
        (43, Some(100), "one"),
        (42, Some(102), "one"),
        (42, Some(100), "two"),
    ] {
        assert!(!timeline.set_account_progress(pid, birth, Some(owner), Some(progress.clone())));
        assert!(timeline.claude_footer().unwrap().account_progress_busy);
    }
    assert!(timeline.set_account_progress(42, Some(100), Some("one"), Some(progress.clone())));
    assert!(!timeline.claude_footer().unwrap().account_progress_busy);
    for (pid, birth, owner) in [
        (43, Some(100), "one"),
        (42, Some(102), "one"),
        (42, Some(100), "two"),
    ] {
        assert!(!timeline.set_account_progress_busy(pid, birth, Some(owner)));
        assert!(!timeline.claude_footer().unwrap().account_progress_busy);
    }
    timeline.set_account_progress_busy(42, Some(100), Some("one"));
    timeline.set_claude_footer(42, Some(102), Some(facts("two")));
    assert!(!timeline.claude_footer().unwrap().account_progress_busy);
    assert_eq!(timeline.claude_footer().unwrap().account_progress, None);
}

#[test]
fn transcript_only_recovery_reads_remain_unknown_and_foreground_bound() {
    use crate::harness_recovery_verb::{cmd, snapshot};
    use aterm_agent::harness::{account_progress::ProgressPhase, session_usage::Wall};
    let mut timeline = SessionTimeline::for_test();
    timeline.agent.program_pgid = 42;
    let mut footer = facts("one");
    footer.usage.get_or_insert_with(Default::default).wall = Some(Wall {
        label: "5h",
        resets: "3pm".into(),
    });
    assert!(timeline.set_claude_footer(42, Some(100), Some(footer)));
    let progress = snapshot(&timeline).expect("typed wall is disclosed");
    assert_eq!(progress.phase, ProgressPhase::Unknown);
    assert_eq!(
        (
            progress.observed_at,
            progress.next_at,
            progress.known,
            progress.eligible
        ),
        (0, None, None, None)
    );
    let reply = cmd("recovery", true, || Ok(snapshot(&timeline)));
    for field in [
        "available=1",
        "phase=unknown",
        "account=-",
        "known=-",
        "eligible=-",
        "observed_at=-",
        "next_at=-",
    ] {
        assert!(
            reply.split_whitespace().any(|word| word == field),
            "{reply}"
        );
    }
    let limit = reply
        .split_whitespace()
        .find_map(|word| word.strip_prefix("wall="))
        .unwrap();
    assert!(aterm_control::wire::pct_decode(limit).contains("provider reset: 3pm"));
    assert!(timeline.claude_footer().unwrap().account_progress.is_none());
    timeline.agent.program_pgid = 43;
    assert_eq!(snapshot(&timeline), None);
    timeline.agent.program_pgid = 42;
    assert!(timeline.set_claude_footer(42, Some(102), Some(facts("two"))));
    assert_eq!(
        snapshot(&timeline),
        None,
        "the old limit is not retained in the new process"
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn bootstrap_stop_census_keeps_stage_and_environment_kind() {
    let cause = |pid| {
        format!(
            "sample exit=75\nwatch bootstrap stopped: code=Some(75) kind=environment condition=refusal for {pid}"
        )
    };
    assert_eq!(summary(&cause(41)), "watch bootstrap environment");
    assert_eq!(summary(&cause(41)), summary(&cause(42)));
    assert_eq!(
        summary(
            "sample exit=75\nwatch stage=tree stopped: code=Some(75) kind=run-stop condition=stopping"
        ),
        "watch stage=tree run-stop"
    );
}

#[test]
fn preemption_census_ranks_conditions_across_distinct_senders() {
    let ledger = |pid| {
        format!(
            "tree: metrics\ncapacity background preemption: signal=15 sender_pid={pid}; ledger observation failed: planted refusal\nverify: COULD NOT RUN — siblings unrun"
        )
    };
    let release = "tree: metrics\ncapacity background preemption: signal=15 sender_pid=43; Release claim pid=44 what=ledger observation failed: test text\nverify: COULD NOT RUN — siblings unrun";
    assert_eq!(summary(&ledger(41)), "capacity ledger observation failed");
    assert_eq!(summary(&ledger(42)), summary(&ledger(41)));
    assert_eq!(summary(release), "Release claim preemption");
    assert_ne!(summary(release), summary(&ledger(41)));
    assert_eq!(
        summary(
            "capacity background stopped before admission: ledger observation failed: planted; detail"
        ),
        "capacity ledger observation failed"
    );
    assert_eq!(
        summary("tree: metrics\naterm-verify: the child was killed by signal 15"),
        "child signal=15"
    );
}

#[test]
fn named_cause_wins_and_historical_absence_is_not_attributed() {
    let mut record: Record = aterm_toml::from_str("commit='c'\ntree='t'\nhost='h'\nverifier=''\ninputs=''\ntoken='k'\nstate='cancelled'\nstarted=0\nupdated=1\nlease_until=0\nrows=[]").unwrap();
    assert!(cause(&record).starts_with("unknown"));
    record.cause = "sample signal=15; sender unavailable".into();
    assert_eq!(cause(&record), record.cause);
}

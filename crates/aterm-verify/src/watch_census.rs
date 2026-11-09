// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Snapshot census; never interprets a missing cause as evidence of a particular sender.

use crate::judged::{self, Record};
use std::collections::BTreeMap;
use std::path::Path;

fn cause(record: &Record) -> String {
    if !record.cause.is_empty() {
        return record.cause.clone();
    }
    let evidence = record
        .rows
        .iter()
        .filter(|row| row.outcome == "could-not-run")
        .map(|row| format!("{}: {}", row.id, row.log))
        .collect::<Vec<_>>()
        .join("\n");
    if evidence.is_empty() {
        "unknown (historical record has no cause)".into()
    } else {
        evidence
    }
}

fn summary(why: &str) -> String {
    if let Some(preemption) = why
        .lines()
        .find(|line| line.starts_with("capacity background "))
    {
        let condition =
            if let Some(sender) = preemption.strip_prefix("capacity background preemption: ") {
                sender
                    .split_once("; ")
                    .map_or(sender, |(_, condition)| condition)
            } else {
                preemption
                    .strip_prefix("capacity background stopped before admission: ")
                    .unwrap_or(preemption)
            };
        if condition.starts_with("ledger observation failed:") {
            return "capacity ledger observation failed".into();
        }
        if condition.starts_with("Release claim pid=") {
            return "Release claim preemption".into();
        }
        return preemption.into();
    }
    if let Some(stopped) = why.lines().find(|line| line.starts_with("watch ")) {
        if let Some((scope, detail)) = stopped.split_once(" stopped: ") {
            let kind = detail
                .split_once(" kind=")
                .and_then(|(_, tail)| tail.split_whitespace().next())
                .unwrap_or("stopped");
            return format!("{scope} {kind}");
        }
        return stopped.into();
    }
    if let Some(signal) = why.lines().rev().find_map(|line| {
        line.split_once("killed by signal ")
            .and_then(|(_, value)| value.split_whitespace().next())
    }) {
        return format!("child signal={signal}");
    }
    why.lines()
        .rev()
        .find(|line| line.contains("COULD NOT RUN"))
        .or_else(|| why.lines().next())
        .unwrap_or("unknown")
        .to_owned()
}

pub(super) fn run(root: &Path, head: &str) -> Result<(), String> {
    // Resolve once so a moving local ref cannot mix snapshots.
    let head = judged::git(
        root,
        &["rev-parse", "--verify", &format!("{head}^{{commit}}")],
    )?;
    let records = judged::records(root, Some(&head), judged::deadline())?;
    let mut states = BTreeMap::<String, usize>::new();
    let mut causes = BTreeMap::<String, usize>::new();
    for record in &records {
        *states.entry(record.state.clone()).or_default() += 1;
        if matches!(record.state.as_str(), "cancelled" | "could-not-run") {
            let why = cause(record);
            // Prefer the tool's refusal over the timing prefix on historical stage rows.
            let summary = summary(&why);
            *causes
                .entry(format!("{}: {}", record.state, summary))
                .or_default() += 1;
            println!(
                "tree={} commit={} state={} cause={why:?}",
                record.tree, record.commit, record.state
            );
        }
    }
    let completed =
        states.get("pass").copied().unwrap_or(0) + states.get("red").copied().unwrap_or(0);
    println!(
        "journal={head} trees={} completed={completed} states={states:?}",
        records.len()
    );
    let mut ranked: Vec<_> = causes.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    for (why, count) in ranked {
        println!("count={count} cause={why:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
}

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
            let summary = why
                .lines()
                .rev()
                .find(|line| line.contains("COULD NOT RUN"))
                .or_else(|| why.lines().next())
                .unwrap_or("unknown");
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
    fn named_cause_wins_and_historical_absence_is_not_attributed() {
        let mut record: Record = aterm_toml::from_str("commit='c'\ntree='t'\nhost='h'\nverifier=''\ninputs=''\ntoken='k'\nstate='cancelled'\nstarted=0\nupdated=1\nlease_until=0\nrows=[]").unwrap();
        assert!(cause(&record).starts_with("unknown"));
        record.cause = "sample signal=15; sender unavailable".into();
        assert_eq!(cause(&record), record.cause);
    }
}

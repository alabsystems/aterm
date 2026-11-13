// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! B8 evidence at cut selection. It informs the operator and never selects or gates a base.

use serde::Deserialize;
use std::path::Path;

const JOURNAL: &str = "refs/aterm/judged";

#[derive(Deserialize)]
struct Verdict {
    commit: String,
    tree: String,
    host: String,
    state: String,
    updated: u64,
}

fn describe(base: &str, tree: &str, journal: &str, text: &str, now: u64) -> Result<String, String> {
    let verdict: Verdict = aterm_toml::from_str(text).map_err(|e| e.to_string())?;
    if verdict.tree != tree {
        return Err("lane record is filed under another tree".into());
    }
    let age = now
        .checked_sub(verdict.updated)
        .ok_or("lane timestamp is in the future")?;
    let state = match verdict.state.as_str() {
        "pass" => "green",
        "red" => "red",
        // A claim, cancellation or unavailable judgment must never look like a verdict.
        other => {
            return Ok(format!(
                "main lane: base {base} tree {tree} has no terminal verdict (state={other}, host={}); journal={JOURNAL}@{journal}",
                verdict.host
            ));
        }
    };
    Ok(format!(
        "main lane: base {base} tree {tree}: {state}, age_s={age}, host={}, sampled_commit={}; record={JOURNAL}@{journal}:verdicts/{tree}.toml (advisory)",
        verdict.host, verdict.commit
    ))
}

pub(crate) fn observe(root: &Path, base: &str, tree: &str, now: impl FnOnce() -> u64) -> String {
    let read = || -> Result<String, String> {
        let remote = crate::github_remote::Remote::origin(root)?;
        let Some(journal) = remote.get(JOURNAL)? else {
            return Ok(format!(
                "main lane: base {base} tree {tree}: journal absent (advisory)"
            ));
        };
        let Some(bytes) = remote.lane_record(&journal, tree)? else {
            return Ok(format!(
                "main lane: base {base} tree {tree}: unjudged; journal={JOURNAL}@{journal} (advisory)"
            ));
        };
        let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
        // A lane may complete during the remote read. Age starts at observation, not request.
        describe(base, tree, &journal, text, now())
    };
    read().unwrap_or_else(|why| {
        format!("main lane: base {base} tree {tree}: evidence unavailable: {why} (advisory)")
    })
}

/// How many of main's newest commits past a line's head are asked for a lane record: one read
/// each, so the pump's advisory stays a bounded cost.
pub(crate) const CANDIDATE_WINDOW: usize = 64;

/// The newest of `commits` (each with its tree, newest first) whose lane record is a pass on
/// that exact tree. A record of another tree, a claim, a red and an unjudged commit are passed
/// over; a read that fails ends the search with what it could not answer.
pub(crate) fn newest_pass_of(
    commits: &[(String, String)],
    mut record: impl FnMut(&str) -> Result<Option<String>, String>,
) -> Result<Option<String>, String> {
    for (commit, tree) in commits {
        let Some(text) = record(tree)? else {
            continue;
        };
        let verdict: Verdict = aterm_toml::from_str(&text).map_err(|e| e.to_string())?;
        if verdict.tree == *tree && verdict.state == "pass" {
            return Ok(Some(commit.clone()));
        }
    }
    Ok(None)
}

/// The newest commit of origin/main past `head`, among its newest [`CANDIDATE_WINDOW`], that
/// the main lane judged a pass: a candidate for a session's `--commit REV --supersede`, never
/// a base the release selects by itself. `None` where there is none or the lane cannot be read.
pub(crate) fn newest_pass(root: &Path, head: &str) -> Option<String> {
    let git = crate::ledger::GitCli::new(root);
    let listed = crate::ledger::git_ok(
        &git,
        &[
            "log",
            "--format=%H %T",
            "-n",
            &CANDIDATE_WINDOW.to_string(),
            "--ancestry-path",
            &format!("{head}..origin/main"),
        ],
    )
    .ok()?
    .stdout_utf8();
    let commits: Vec<(String, String)> = listed
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(commit, tree)| (commit.to_string(), tree.to_string()))
        .collect();
    if commits.is_empty() {
        return None;
    }
    let remote = crate::github_remote::Remote::origin(root).ok()?;
    let journal = remote.get(JOURNAL).ok()??;
    newest_pass_of(&commits, |tree| {
        let Some(bytes) = remote.lane_record(&journal, tree)? else {
            return Ok(None);
        };
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|e| e.to_string())
    })
    .ok()
    .flatten()
}

#[cfg(test)]
#[path = "main_lane_tests.rs"]
mod tests;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_selected_tree_gets_a_terminal_verdict_and_its_age() {
        let record = |tree: &str, state: &str| {
            format!(
                "commit = 'sample'\ntree = '{tree}'\nhost = 'm112'\nstate = '{state}'\nupdated = 100\n"
            )
        };
        for (state, expected) in [("pass", "green"), ("red", "red")] {
            let text = describe("base", "tree", "journal", &record("tree", state), 123).unwrap();
            assert!(text.contains(&format!(": {expected}, age_s=23, host=m112")));
            assert!(text.contains("sampled_commit=sample"));
            assert!(text.contains("refs/aterm/judged@journal:verdicts/tree.toml"));
            assert!(describe("base", "other", "journal", &record("tree", state), 123).is_err());
        }
        for state in ["judging", "cancelled", "could-not-run"] {
            let text = describe("base", "tree", "j", &record("tree", state), 123).unwrap();
            assert!(text.contains("no terminal verdict"));
            assert!(!text.contains("age_s="));
        }
        assert!(describe("base", "tree", "j", &record("tree", "pass"), 99).is_err());
    }
}

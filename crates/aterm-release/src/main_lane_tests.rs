// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

/// THE SUPERSEDE CANDIDATE IS THE NEWEST PASS ON ITS OWN TREE. Of main's commits past a line's
/// head, newest first, the first whose lane record is a terminal pass filed under that commit's
/// tree is named; a red, a claim, an unjudged commit and a record filed under another tree are
/// passed over, and a lane that cannot be read names nothing.
#[test]
fn the_newest_pass_on_its_own_tree_is_the_candidate() {
    let record = |tree: &str, state: &str| {
        Some(format!(
            "commit = 'sample'\ntree = '{tree}'\nhost = 'm45'\nstate = '{state}'\nupdated = 100\n"
        ))
    };
    let commits: Vec<(String, String)> = ["c5", "c4", "c3", "c2", "c1"]
        .iter()
        .map(|c| ((*c).to_string(), format!("tree-{c}")))
        .collect();
    let lane = |tree: &str| -> Result<Option<String>, String> {
        Ok(match tree {
            "tree-c5" => None,
            "tree-c4" => record(tree, "red"),
            "tree-c3" => record("tree-of-another", "pass"),
            "tree-c2" => record(tree, "pass"),
            "tree-c1" => record(tree, "pass"),
            _ => unreachable!("{tree}"),
        })
    };
    assert_eq!(newest_pass_of(&commits, lane), Ok(Some("c2".into())));
    assert_eq!(newest_pass_of(&commits[..3], lane), Ok(None));
    for unsettled in ["judging", "cancelled", "could-not-run"] {
        assert_eq!(
            newest_pass_of(&commits[..1], |tree| Ok(record(tree, unsettled))),
            Ok(None),
            "{unsettled}"
        );
    }
    let mut asked = Vec::new();
    let down = newest_pass_of(&commits, |tree| {
        asked.push(tree.to_string());
        Err("origin did not answer".into())
    });
    assert_eq!(down, Err("origin did not answer".into()));
    assert_eq!(asked, ["tree-c5"], "an unread lane ends the search");
}

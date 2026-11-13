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

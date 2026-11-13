// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::RunOut;

const HEAD: &str = "2617295d1111111111111111111111111111aaaa";
const OLDER: &str = "15f2b5b6bc51d9ef56e5e4fa50f434fca07f40ee";

struct Claimed {
    parent: &'static str,
    changed: &'static str,
}

impl GitRunner for Claimed {
    fn git(&self, args: &[&str]) -> Result<RunOut> {
        let text = match args.first() {
            Some(&"rev-parse") => self.parent,
            Some(&"diff") => self.changed,
            _ => "",
        };
        Ok(RunOut {
            status: 0,
            stdout: text.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }
}

fn unrelated() -> Claimed {
    Claimed {
        parent: OLDER,
        changed: "crates/aterm-release/src/publish.rs\n",
    }
}

#[test]
fn another_commits_binary_is_refused() {
    own_cutter_with(&unrelated(), HEAD, HEAD).expect("its own");
    own_cutter_with(&unrelated(), OLDER, HEAD).expect_err("not its own");
    let docs = Claimed {
        parent: OLDER,
        changed: "docs/NOTE.md\n",
    };
    assert!(own_cutter_with(&docs, OLDER, HEAD).is_err());
}

#[test]
fn a_claim_commit_is_finished_by_the_cutter_of_the_commit_it_claims() {
    let claim = Claimed {
        parent: OLDER,
        changed: "CHANGELOG.md\nRELEASES.ledger\n",
    };
    own_cutter_with(&claim, OLDER, HEAD).expect("the claim's cutter");
    let recut = Claimed {
        parent: OLDER,
        changed: "RELEASES.ledger\n",
    };
    assert!(
        own_cutter_with(&recut, OLDER, HEAD).is_ok(),
        "a recut rolls no section"
    );
    let more = Claimed {
        parent: OLDER,
        changed: "CHANGELOG.md\nRELEASES.ledger\ncrates/aterm-release/src/gates.rs\n",
    };
    assert!(own_cutter_with(&more, OLDER, HEAD).is_err());
    let other_parent = Claimed {
        parent: "3333333333333333333333333333333333333333",
        changed: "RELEASES.ledger\n",
    };
    assert!(own_cutter_with(&other_parent, OLDER, HEAD).is_err());
}

#[test]
fn a_binary_that_is_no_commits_build_fails_closed() {
    let claim = Claimed {
        parent: "unknown",
        changed: "RELEASES.ledger\n",
    };
    let none = crate::cutter::NotOwn {
        why: "its checkout is dirty".into(),
        fix: "commit it".into(),
    };
    let not = own_cutter_of(&claim, Err(&none), HEAD).expect_err("none");
    assert!(not.why.ends_with(&none.why), "{not:?}");
    assert_eq!(not.fix, none.fix, "the verdict's own remedy");
    own_cutter_with(&claim, "unknown", HEAD).expect_err("not an id");
}

/// A frozen cutter's content proof covers its commit and that commit's claim commit,
/// which adds only CLAIM_FILES (no cutter compiles them in): a resume past the claim runs
/// R's frozen cutter against the claim commit (aterm 0.184.0's step 8 resume refused
/// here). Any other child, or another parent, is still refused.
#[test]
fn a_content_proved_cutter_finishes_only_its_own_claim_commit() {
    let claim = Claimed {
        parent: OLDER,
        changed: "CHANGELOG.md\nRELEASES.ledger\n",
    };
    let proved = crate::cutter::Stamp::Proved(HEAD.into());
    assert!(own_cutter_stamp_of(&claim, Ok(&proved), HEAD).is_ok());

    let proved_old = crate::cutter::Stamp::Proved(OLDER.into());
    let own_claim = own_cutter_stamp_of(&claim, Ok(&proved_old), HEAD)
        .expect("R's frozen cutter finishes R's claim commit");
    assert!(own_claim.contains("content-proved"), "{own_claim}");

    let more = Claimed {
        parent: OLDER,
        changed: "CHANGELOG.md\nRELEASES.ledger\ncrates/aterm-release/src/gates.rs\n",
    };
    let not = own_cutter_stamp_of(&more, Ok(&proved_old), HEAD).unwrap_err();
    assert!(not.why.contains("content-proved"), "{not:?}");
    let other_parent = Claimed {
        parent: "3333333333333333333333333333333333333333",
        changed: "RELEASES.ledger\n",
    };
    assert!(own_cutter_stamp_of(&other_parent, Ok(&proved_old), HEAD).is_err());
    assert!(
        own_cutter_stamp_of(&unrelated(), Ok(&proved_old), HEAD).is_err(),
        "a source change is never a claim"
    );

    let built_old = crate::cutter::Stamp::Built(OLDER.into());
    assert!(
        own_cutter_stamp_of(&claim, Ok(&built_old), HEAD).is_ok(),
        "the existing dep-info claim exception remains for hand-run cutters"
    );
}

/// Release qualification crosses the frozen-tool boundary even when the launcher is already
/// an exact dep-info build of R. Hand-run gates retain that build judgment. Mutating the
/// release-only gate to call `own_cutter_stamp_of` makes the first assertion fail.
#[test]
fn release_flow_freezes_even_an_exact_dep_info_build() {
    let exact = crate::cutter::Stamp::Built(HEAD.into());
    let not = own_frozen_cutter_stamp_of(Ok(&exact), HEAD).unwrap_err();
    assert!(
        not.why.contains("not the frozen content-proved cutter"),
        "{not:?}"
    );
    assert!(own_cutter_stamp_of(&unrelated(), Ok(&exact), HEAD).is_ok());

    let proved = crate::cutter::Stamp::Proved(HEAD.into());
    assert!(own_frozen_cutter_stamp_of(Ok(&proved), HEAD).is_ok());
}

#[test]
fn only_a_dry_run_proceeds_with_another_commits_cutter() {
    let other = "4444444444444444444444444444444444444444";
    assert!(own_cutter_gate(&unrelated(), other, true).is_ok());
    own_cutter_gate(&unrelated(), other, false).expect_err("refused");
}

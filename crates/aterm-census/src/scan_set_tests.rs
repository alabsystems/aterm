// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::scan_set::test_fixtures::synth_tree;

#[test]
fn the_forge_package_roster_comes_from_both_authoritative_registries() {
    let files = test_fixtures::workspace_manifests(&[]);
    let root = test_fixtures::disk_tree("aterm-fork-packages", "both", &files);
    let packages = fork_packages(&root).expect("the fixture registries parse");
    assert!(packages.contains(&test_fixtures::FIXTURE_FORK.0.to_string()));
    for row in ABSORBED_CRATES {
        assert!(
            packages.contains(&test_fixtures::member_name(row).to_string()),
            "{} is in the absorbed registry: {packages:?}",
            row.path
        );
    }
    let absent = test_fixtures::synth_tree("aterm-fork-packages", "absent", &[]);
    assert!(
        fork_packages(&absent).is_err(),
        "a missing authority must pull Forge fail-closed, not look like an empty roster"
    );
}

#[test]
fn a_patch_target_outside_vendor_and_crates_fails_closed() {
    let root = synth_tree("aterm-scan-set", "oddpatch", &[]);
    let err =
        classify_patch_target("elsewhere", "third_party/elsewhere", &root).expect_err("must fail");
    assert!(
        err.contains("under neither `vendor/`") && err.contains("fail-closed"),
        "err: {err}"
    );
}

#[test]
fn a_redistribution_under_crates_is_refused_not_called_first_party() {
    let root = synth_tree(
        "aterm-scan-set",
        "redistribution",
        &[
            (
                "crates/aterm-fork/LICENSE-MIT".to_string(),
                "MIT\n".to_string(),
            ),
            (
                "crates/aterm-noticed/src/lib.rs".to_string(),
                "// derived\n".to_string(),
            ),
            (
                "NOTICE".to_string(),
                "crates/aterm-noticed is derived from upstream\n".to_string(),
            ),
        ],
    );
    let license = classify_patch_target("fork", "crates/aterm-fork", &root);
    let notice = classify_patch_target("noticed", "crates/aterm-noticed", &root);
    root.write("crates/aterm-noticed/LICENSE", "upstream\n");
    let both = classify_patch_target("noticed", "crates/aterm-noticed", &root);
    assert_eq!(both, Ok(PatchTargetKind::Absorbed));
    let license = license.expect_err("a retained license is a redistribution");
    assert!(
        license.contains("crates/aterm-fork/LICENSE-MIT"),
        "{license}"
    );
    let notice = notice.expect_err("a NOTICE line is a redistribution");
    assert!(notice.contains("NOTICE"), "{notice}");
}

#[test]
fn a_genuine_first_party_crate_is_not_mistaken_for_a_redistribution() {
    let root = synth_tree(
        "aterm-scan-set",
        "own",
        &[
            (
                "crates/aterm-own/src/lib.rs".to_string(),
                "// original\n".to_string(),
            ),
            (
                "NOTICE".to_string(),
                "crates/aterm-other is derived from upstream\n".to_string(),
            ),
        ],
    );
    let evidence = redistribution_evidence(&root, "crates/aterm-own");
    let kind = classify_patch_target("own", "crates/aterm-own", &root);
    assert_eq!(evidence, None);
    assert_eq!(kind, Ok(PatchTargetKind::FirstParty));
}

#[test]
fn the_review_set_is_the_fork_ledger() {
    let repo = test_fixtures::repo_root();
    for fork in reviewed_forks(&repo).expect("the real ledger reads") {
        assert!(
            repo.join(&fork.path).join("Cargo.toml").is_file(),
            "{fork:?}"
        );
    }
    let read = |name: &str, ledger: &str| {
        reviewed_forks(&synth_tree(
            "aterm-scan-set",
            name,
            &[(FORK_LEDGER.to_string(), ledger.to_string())],
        ))
    };
    let good = test_fixtures::ledger(&[("a", "vendor/a")]);
    assert_eq!(
        read("ledger-good", &good).expect("a ledger reads"),
        [ReviewedFork {
            package: "a".to_string(),
            path: "vendor/a".to_string(),
        }]
    );
    let err =
        read("ledger-no-path", &good.replace("path = \"vendor/a\"\n", "")).expect_err("must fail");
    assert!(err.contains("no `name` or no `path`"), "{err}");
    let root = synth_tree("aterm-scan-set", "ledger-absent", &[]);
    assert_eq!(reviewed_forks(&root), Ok(Vec::new()));
}

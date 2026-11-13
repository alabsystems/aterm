// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::EnvSnapshot;

/// Removing the mode boundary judges the wrong section during a recut. The dev control
/// still refuses an empty Unreleased; the release's selected rolled section is valid.
#[test]
fn a_recut_leaves_section_selection_to_the_release_gate() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let text = "## [Unreleased]\n\n## [0.192.0] - 2026-11-09\n\n### Fixed\n\n- Shipped note.\n";
    std::fs::write(dir.path().join(CHANGELOG_FILE), text).unwrap();
    let mut ctx = Ctx::new(
        dir.path().to_path_buf(),
        Scope::workspace(),
        EnvSnapshot::default(),
        dir.path().join("scratch"),
    );
    ctx.input_paths = Some(vec![CHANGELOG_FILE.into()]);
    assert!(
        judge(&ctx)
            .unwrap()
            .unwrap()
            .iter()
            .any(|s| s.contains("no release notes"))
    );
    ctx.release = true;
    assert!(judge(&ctx).unwrap().is_none());
    assert!(aterm_census::changelog::gate_section(text, "0.192.0").is_ok());
}

/// The real diff selector includes committed, staged and unstaged changelog edits; an
/// unrelated file leaves even a broken changelog unjudged. Removing either selection or
/// structural judgment breaks one of these controls.
#[test]
fn only_changes_touching_the_changelog_are_judged() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    let env = EnvSnapshot {
        path: std::env::var_os("PATH").unwrap_or_default(),
        ..Default::default()
    };
    // The fixture producer and diff selector use the same explicit Git provider.
    let git = |args: &[&str]| {
        let out = crate::changed::git(root, &env.path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "A"]);
    git(&["config", "user.email", "a@example.invalid"]);
    let good = "## [Unreleased]\n\n### Fixed\n\n- Kept note.\n";
    std::fs::write(root.join(CHANGELOG_FILE), good).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\n[workspace.package]\nversion = \"0.192.0\"\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    git(&["branch", "base"]);
    git(&["tag", "v0.191.0"]);
    let ctx = Ctx::new(
        root.to_path_buf(),
        Scope::changed("base", vec![], false),
        env.clone(),
        root.join("scratch"),
    );
    std::fs::write(root.join("unrelated.txt"), "work").unwrap();
    assert!(judge(&ctx).unwrap().is_none());
    for bad in [
        format!("- Stray.\n\n{good}"),
        format!("{good}\n### Fixed\n\n- Second note.\n"),
        format!(
            "{good}\n{HARNESS_ENTRY_HEAD}0.1.0: tool.**\n\n{HARNESS_ENTRY_HEAD}0.1.0: tool.**\n"
        ),
    ] {
        std::fs::write(root.join(CHANGELOG_FILE), &bad).unwrap();
        // No public baseline is required to report structural errors.
        let out = judge(&ctx).unwrap().unwrap();
        assert!(!out.is_empty(), "invalid changed changelog passed");
    }
    let harness = format!("{good}\n{HARNESS_ENTRY_HEAD}0.191.0: tool.**\n");
    std::fs::write(root.join(CHANGELOG_FILE), &harness).unwrap();
    assert!(judge(&ctx).unwrap().unwrap().is_empty());
    std::fs::write(
        root.join(CHANGELOG_FILE),
        harness.replace("since v0.191.0", "since v0.188.0"),
    )
    .unwrap();
    assert!(
        judge(&ctx)
            .unwrap()
            .unwrap()
            .iter()
            .any(|s| s.contains("previous public release"))
    );
    for (text, fails) in [
        (
            include_str!("../../aterm-census/tests/fixtures/changelog-before.md"),
            true,
        ),
        (
            include_str!("../../aterm-census/tests/fixtures/changelog-repaired.md"),
            false,
        ),
    ] {
        std::fs::write(root.join(CHANGELOG_FILE), text).unwrap();
        let found = judge(&ctx).unwrap().unwrap();
        assert_eq!(!found.is_empty(), fails);
        if fails {
            for defect in [
                "above the first",
                "duplicate ### Fixed",
                "out of Keep a Changelog order",
                "glued",
                "duplicate Harness",
                "previous public release",
            ] {
                assert!(
                    found.iter().any(|s| s.contains(defect)),
                    "missing {defect}: {found:?}"
                );
            }
        }
    }
    std::fs::write(
        root.join(CHANGELOG_FILE),
        format!("{good}\n- Another note.\n"),
    )
    .unwrap();
    git(&["add", CHANGELOG_FILE]);
    assert!(judge(&ctx).unwrap().unwrap().is_empty());
    git(&["commit", "-qm", "valid note"]);
    assert!(judge(&ctx).unwrap().unwrap().is_empty());
}

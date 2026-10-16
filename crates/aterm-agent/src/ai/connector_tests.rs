// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn every_row_is_a_package_and_the_connections_follow_its_order() {
    let programs: Vec<&str> = atpkg::vendor_direct::VENDORS
        .iter()
        .map(|v| v.program)
        .collect();
    for row in ROWS {
        assert!(
            programs.contains(&row.id),
            "{} is no VENDORS program",
            row.id
        );
    }
    let order: Vec<&str> = programs
        .iter()
        .copied()
        .filter(|p| connector(p).is_some())
        .collect();
    assert_eq!(ids(), order);
}

#[test]
fn a_measured_reviewer_ignores_the_reviewed_trees_own_settings() {
    // Claude: `-p` skips the folder-trust dialog, so without these a repository's
    // `.claude/settings.json` could route the request and its token elsewhere.
    for flag in [
        "--restricted",
        "--safe-mode",
        "--strict-mcp-config",
        "--no-session-persistence",
    ] {
        assert!(
            CLAUDE.review.args.contains(&flag),
            "claude reviews without {flag}"
        );
    }
    let tools = CLAUDE
        .review
        .args
        .iter()
        .position(|a| *a == "--tools")
        .expect("--tools");
    assert_eq!(CLAUDE.review.args[tools + 1], "Read,Grep,Glob");
    // Codex: from an empty directory, its user config and rules ignored, read-only.
    for flag in ["--ignore-user-config", "--ignore-rules", "--ephemeral"] {
        assert!(
            CODEX.review.args.contains(&flag),
            "codex reviews without {flag}"
        );
    }
    let sandbox = CODEX
        .review
        .args
        .iter()
        .position(|a| *a == "--sandbox")
        .expect("--sandbox");
    assert_eq!(CODEX.review.args[sandbox + 1], "read-only");
    assert_eq!(CODEX.review.scope, Scope::DiffOnly);
    // Its sandbox reads any file this user can: it reviews with no tool at all.
    for off in [
        "features.shell_tool=false",
        "features.unified_exec=false",
        "features.view_image=false",
    ] {
        assert!(
            CODEX.review.args.contains(&off),
            "codex reviews with {off} on"
        );
    }
}

#[test]
fn claude_signs_in_only_through_accounts_and_codex_only_when_signed_out() {
    assert!(CLAUDE.is_claude());
    assert!(matches!(
        CODEX.login,
        Login::Run {
            signs_out_first: true,
            ..
        }
    ));
    for row in [&AGY, &MUSE] {
        assert!(!row.measured, "{} is not measured yet", row.id);
        assert_eq!(row.review.scope, Scope::DiffOnly);
        assert!(!matches!(row.review.prompt, PromptVia::Stdin { .. }) || row.id == "codex");
    }
}

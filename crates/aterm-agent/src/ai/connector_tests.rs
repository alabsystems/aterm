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
fn every_reviewer_keeps_all_its_capabilities() {
    // Batteries included (owner ruling 2026-10-16): no row switches a tool, a setting, a
    // hook, an MCP server or a session off; the permission level is the asker's (`levels`).
    for row in ROWS {
        for flag in [
            "--restricted",
            "--safe-mode",
            "--tools",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--ignore-user-config",
            "--ignore-rules",
            "--ephemeral",
            "--disable-shell",
            "--disable-write",
            "--disable-web-tools",
            "--disable-slash-commands",
        ] {
            assert!(
                !row.review.args.contains(&flag),
                "{} reviews with {flag}",
                row.id
            );
        }
        assert!(
            !row.review.args.iter().any(|a| a.contains("=false")),
            "{} switches a feature off: {:?}",
            row.id,
            row.review.args
        );
        assert!(
            !row.review.levels.yolo.is_empty(),
            "{} has no yolo flag",
            row.id
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
        assert!(!matches!(row.review.prompt, PromptVia::Stdin { .. }) || row.id == "codex");
    }
}

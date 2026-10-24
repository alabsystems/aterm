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
fn a_pooled_row_atpkg_does_not_serve_is_no_connection() {
    for row in [&GROK, &KIMI] {
        assert!(!ROWS.iter().any(|r| r.id == row.id), "{} is a row", row.id);
        assert_eq!(connector(row.id).map(|c| c.id), None);
        assert!(!connections().any(|c| c.id == row.id), "{}", row.id);
        assert!(!ids().contains(&row.id));
        assert!(!row.measured, "no lab pass ran {}'s commands", row.id);
    }
    assert_eq!(connector("claude").map(|c| c.id), Some("claude"));
    assert_eq!(connector("nope").map(|c| c.id), None);
}

#[test]
fn every_pooled_cli_names_its_row_and_its_own_roster_store_and_seam() {
    assert_eq!(Cli::ids(), ["claude", "codex", "grok", "kimi"]);
    for cli in Cli::ALL {
        assert_eq!(Cli::parse(cli.id()), Some(cli));
        assert!(std::ptr::eq(
            cli.connector().accounts.expect("a pool"),
            cli.accounts()
        ));
    }
    assert_eq!(Cli::parse("agy"), None, "agy has no pool");
    assert_eq!(Cli::parse("Claude"), None);
    let claude = Cli::Claude.accounts();
    assert_eq!(
        (claude.roster_file, claude.store_dir, claude.seam.var()),
        (
            "accounts.toml",
            "accounts",
            "CLAUDE_SECURESTORAGE_CONFIG_DIR"
        )
    );
    let files: Vec<&str> = Cli::ALL.iter().map(|c| c.accounts().roster_file).collect();
    assert_eq!(
        files,
        [
            "accounts.toml",
            "accounts-codex.toml",
            "accounts-grok.toml",
            "accounts-kimi.toml"
        ]
    );
    assert_eq!(
        Cli::Grok.accounts().seam,
        Seam::LoginFile {
            var: "GROK_AUTH_PATH",
            file: "auth.json"
        }
    );
    for cli in [Cli::Codex, Cli::Kimi] {
        let Seam::Overlay { var, shared } = cli.accounts().seam else {
            panic!("{} is an overlay home", cli.id());
        };
        assert_eq!(Some(var), cli.connector().home.var);
        assert!(shared.contains(&Shared::Dir("sessions")), "{}", cli.id());
    }
    assert!(
        !Cli::Kimi.accounts().measured,
        "resume across homes is unmeasured"
    );
}

#[test]
fn codex_reads_a_configured_api_key_and_its_new_auth_and_limit_words() {
    use super::super::login::{Said, read_status};
    use super::super::testing::ran;
    let Login::Run {
        after: Some(status),
        ..
    } = &CODEX.login
    else {
        panic!("codex reads its status")
    };
    let read = |line: &str| read_status(status, &Ok(ran(0, "", line)));
    assert_eq!(read("API key configured - sk-****\n"), Said::In("api-key"));
    assert_eq!(read("Not logged in\n"), Said::Out);
    assert_eq!(read("Logged in using ChatGPT\n"), Said::In("chatgpt"));
    assert_eq!(
        read("Logged in using an API key - sk-****\n"),
        Said::In("api-key")
    );
    let walls = &Cli::Codex.accounts().walls;
    assert!(walls.limit_in("Your workspace is out of credits."));
    assert!(walls.limit_in("You've hit your usage limit. Try again at 3pm."));
    assert!(!walls.limit_in("error: stream disconnected before completion"));
    assert!(walls.ended_in("Your refresh token was already used; please sign in again"));
    assert!(!walls.ended_in("error: stream disconnected before completion"));
    assert!(CODEX.limit_words.contains(&"spend cap"));
    assert!(CODEX.auth_words.contains(&"was revoked"));
}

#[test]
fn a_passing_throttle_is_never_a_wall() {
    let kimi = &Cli::Kimi.accounts().walls;
    assert!(kimi.limit_in("Error: You exceeded your current token quota."));
    assert!(kimi.limit_in("Insufficient balance: please recharge"));
    assert!(
        !kimi.limit_in(
            "You've reached your concurrent request limit; insufficient balance is not it"
        )
    );
    let grok = &Cli::Grok.accounts().walls;
    assert!(grok.limit_in("You\u{2019}ve hit the rate limit for your plan"));
    assert!(!grok.limit_in("the rate limit for this endpoint is documented below"));
}

#[test]
fn every_reviewer_keeps_all_its_capabilities() {
    // Batteries included: no row switches a tool, a setting, a
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

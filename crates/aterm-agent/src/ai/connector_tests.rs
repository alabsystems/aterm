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
        let Seam::Overlay { var, shared, .. } = cli.accounts().seam else {
            panic!("{} is an overlay home", cli.id());
        };
        assert_eq!(Some(var), cli.connector().home.var);
        assert!(shared.contains(&Shared::Dir("sessions")), "{}", cli.id());
    }
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
    assert!(!grok.limit_in("The service is busy. Wait a minute and send again."));
}

/// EVERY GROK BUILD WALL THE SCREEN READS IS A LIMIT TO ITS POOL: each `grok.wall.*` anchor
/// aterm-phase holds to the installed binary, and the credit failures the binary's strings list.
/// CONTROL: the upsell's bare `Try Again` label, the `API error (status ` prefix alone and its
/// upgrade option say no limit.
#[test]
fn every_grok_wall_the_screen_reads_is_a_limit_to_its_pool() {
    let grok = &Cli::Grok.accounts().walls;
    let controls = ["grok.wall.try_again", "grok.wall.api_error"];
    let walls: Vec<_> = aterm_phase::anchors::GROK_ANCHORS
        .iter()
        .filter(|a| a.id.starts_with("grok.wall.") && !controls.contains(&a.id))
        .collect();
    assert!(!walls.is_empty(), "the filter keeps the wall anchors");
    for a in walls {
        assert!(grok.limit_in(a.text), "{}: {}", a.id, a.text);
    }
    for line in [
        "402 Payment Required: run out of credits",
        "out of credits or over your spending limit. Add credits and retry.",
        "Ask a team admin to purchase more credits for higher limits, or try again later.",
        "error: usage balance exhausted",
        "error: usage limit reached",
        "error_code: subscription:free-usage-exhausted",
        "error_code: insufficient_quota",
    ] {
        assert!(grok.limit_in(line), "{line}");
    }
    for id in controls {
        let text = aterm_phase::anchors::anchor(id);
        assert!(!grok.limit_in(text), "CONTROL: {id}: {text}");
    }
    assert!(
        !grok.limit_in("Upgrade to a higher tier for more usage"),
        "CONTROL"
    );
}

/// EVERY GROK BUILD LOGIN-ENDED WORD IS ONE OF THE BUILD'S OWN LOGIN-ENDED TEXTS: each
/// `grok.auth.*` anchor (which aterm-phase's `vendor_drift` bench holds to the installed binary,
/// with a mutant of each as its negative control) says the login ended by the pool's words and a
/// review's (which no running path reads yet: Grok Build's row is not in `ROWS`), and each word
/// stands in one of those texts, so no word outlives the build's strings.
/// NEGATIVE: grok 1.0.70's texts that end no login of a row's (a passing failure, an MCP
/// server's or Grove's own credential, the sign-in flow's own ending, a feature an API-key
/// session lacks, a team's deployment key, an admin's API-key policy, a configuration with no
/// sign-in, a model endpoint's own key, a 401 after a renewal) say nothing, and neither does a
/// review's failure tail of them.
#[test]
fn every_grok_auth_word_is_one_of_the_builds_login_ended_texts() {
    let grok = &Cli::Grok.accounts().walls;
    assert_eq!(grok.ended, GROK.auth_words, "one list for both");
    let texts: Vec<_> = aterm_phase::anchors::GROK_ANCHORS
        .iter()
        .filter(|a| a.id.starts_with("grok.auth."))
        .collect();
    assert!(texts.len() > 20, "the filter keeps the login texts");
    // The named sign-in helper's opener is the words before the helper's name.
    let prefix = "grok.auth.not_renewed.named";
    for a in &texts {
        let line = if a.id == prefix {
            format!(
                "{} okta-helper could not renew it in the background.",
                a.text
            )
        } else {
            a.text.to_string()
        };
        assert!(grok.ended_in(&line), "{}: {line}", a.id);
    }
    assert!(
        !grok.ended_in(aterm_phase::anchors::anchor(prefix)),
        "CONTROL: {prefix}"
    );
    for word in GROK.auth_words {
        assert!(
            texts.iter().any(|a| a.text.to_lowercase().contains(word)),
            "{word:?} is in no grok.auth.* text"
        );
    }
    let none = [
        "Authentication is temporarily unavailable. Run `grok login` if this persists.",
        "Token refresh failed transiently (network); skipping browser escalation",
        "Re-authenticated after 401; retrying request",
        "auth: refresh failed but token still valid (grace), using cached",
        "If this is a network or sign-in problem, check your connection or run `grok login`; then run this command again.",
        "Could not load your settings for `grok workspace`. Check your network connection (run `grok login` if you are signed out), then try again.",
        "Access token expired or nearly expired, refreshing.",
        "Token refresh not possible, re-authorization required.",
        "Invalid token type: Token expired",
        "Skipping OAuth MCP in non-interactive mode (stored credentials unusable); re-authenticate in TUI",
        "reconnect: re-authenticate failed",
        "token expired and refresh is off; new session or enable GROVE_TOKEN_ROTATION=expected",
        "Grove Git credentials belong to the Grove daemon, not `grok login`.",
        "Token exchange error: device auth token expired",
        "Device code expired. Run `grok login --device-auth` again.",
        "Device-code login is not available for this deployment. Try `grok login` or set XAI_API_KEY instead.",
        "Billing data requires auth with grok.com. Run `grok login` to authenticate.",
        "Share session is disabled. Run `grok login` to authenticate.",
        "No upload credentials for this account (run `grok login` or set a deployment key); exporting locally.",
        "Your GROK_DEPLOYMENT_KEY is invalid or expired. Please contact a team admin.",
        "The deployment key was rejected. Confirm that GROK_DEPLOYMENT_KEY is set correctly and hasn't expired.",
        "API-key auth is disabled by your administrator. Run `grok login` to authenticate.",
        "No login method available",
        "Authentication failed: the model endpoint rejected this build's credential. Check its key, then resend your message.",
        "Auth recovery succeeded but 3 authenticated inference requests were still rejected (401); giving up after 3 retries.",
        "Log in or re-authenticate with your account",
        "Sign in again",
    ];
    for line in none {
        assert!(!grok.ended_in(line), "{line}");
        let lower = line.to_ascii_lowercase();
        assert!(
            !GROK.auth_words.iter().any(|w| lower.contains(w)),
            "a review reads {line:?} as an ended login"
        );
    }
}

/// A KIMI CODE ROW WHOSE MANAGED LOGIN LOST ITS TOKEN HAS ENDED: kimi 2.3.1's `-p` failure line
/// for it (measured, `docs/measured/kimi-credentials-2026-10-24.md` §5) is an ended login by the
/// pool's words and a review's (which no running path reads yet: Kimi Code's row is not in
/// `ROWS`). NEGATIVE: the same sentence for an environment model with no key or a
/// hand-written provider with none (both measured) is not.
#[test]
fn a_kimi_managed_login_with_no_token_has_ended_and_another_providers_has_not() {
    let kimi = &Cli::Kimi.accounts().walls;
    assert_eq!(kimi.ended, KIMI.auth_words, "one list for both");
    let ended =
        "error: failed to run prompt: provider managed:kimi-code has no credential configured";
    assert!(kimi.ended_in(ended));
    for other in [
        "error: failed to run prompt: provider __kimi_env__ has no credential configured",
        "error: failed to run prompt: provider home has no credential configured",
    ] {
        assert!(!kimi.ended_in(other), "{other}");
        assert!(
            !KIMI.auth_words.iter().any(|w| other.contains(w)),
            "{other}"
        );
    }
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

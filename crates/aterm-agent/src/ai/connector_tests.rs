// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ai::permission::Level;

/// Whether `line` says one of `words`, read as a review's failure tail reads it (lowercased,
/// [`crate::ai::review::verdict_of`]).
fn says(words: &[&str], line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    words.iter().any(|w| line.contains(w))
}

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

/// GROK BUILD AND KIMI CODE ARE CONNECTIONS: atpkg serves both in its default set, and each row
/// is a connection (its review, its sign-in, its doctor row), last in `VENDORS` order. Kimi
/// Code's review is measured (on 2.3.3) and held to the one level its `-p` runs at; Grok Build's
/// is not measured yet.
#[test]
fn grok_and_kimi_are_connections() {
    for row in [&GROK, &KIMI] {
        assert!(
            atpkg::vendor_direct::spec(row.id).is_some(),
            "{} is a VENDORS program",
            row.id
        );
        assert!(ROWS.iter().any(|r| r.id == row.id), "{} is a row", row.id);
        assert!(connections().any(|c| c.id == row.id));
        assert!(ids().contains(&row.id));
    }
    for (row, measured, why) in [
        (&GROK, false, "no lab pass ran grok's commands"),
        (&KIMI, true, "kimi-review-2026-10-27 measured kimi's review"),
    ] {
        assert_eq!(row.measured, measured, "{why}");
    }
    assert_eq!(KIMI.review.only_at, Some(Level::Yolo), "-p is Never Ask");
    assert!(
        ROWS.iter()
            .all(|r| r.id == KIMI.id || r.review.only_at.is_none()),
        "every other row's flags hold it to each level"
    );
    assert_eq!(connector("grok").map(|c| c.product), Some("Grok Build"));
    assert_eq!(connector("kimi").map(|c| c.product), Some("Kimi Code CLI"));
    assert_eq!(
        &ids()[ids().len() - 2..],
        ["grok", "kimi"],
        "last in VENDORS order"
    );
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
        Cli::Codex.accounts().seam,
        Seam::LoginFile {
            var: "CODEX_AUTH_PATH",
            file: "auth.json"
        }
    );
    assert_eq!(
        Cli::Grok.accounts().seam,
        Seam::LoginFile {
            var: "GROK_AUTH_PATH",
            file: "auth.json"
        }
    );
    let cli = Cli::Kimi;
    let Seam::Overlay { var, shared, .. } = cli.accounts().seam else {
        panic!("{} is an overlay home", cli.id());
    };
    assert_eq!(Some(var), cli.connector().home.var);
    assert!(shared.contains(&Shared::Dir("sessions")), "{}", cli.id());
}

#[test]
fn claude_says_its_five_hour_wall_by_either_name_of_the_window() {
    // 2.1.312 names the five-hour window `5-hour limit`, the builds before it `session limit`.
    for wall in [
        "You've hit your 5-hour limit · resets 3pm (America/Los_Angeles)",
        "You've hit your session limit · resets 3pm (America/Los_Angeles)",
    ] {
        assert!(says(CLAUDE.limit_words, wall), "{wall}");
    }
    assert!(!says(
        CLAUDE.limit_words,
        "5-hour limit reset · next reset available Oct 9 at 5am · your weekly limit still applies"
    ));
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
    assert!(says(CODEX.limit_words, "Your workspace is out of credits."));
    assert!(says(
        CODEX.limit_words,
        "You've hit your usage limit. Try again at 3pm."
    ));
    assert!(!says(
        CODEX.limit_words,
        "error: stream disconnected before completion"
    ));
    assert!(says(
        CODEX.auth_words,
        "Your refresh token was already used; please sign in again"
    ));
    assert!(!says(
        CODEX.auth_words,
        "error: stream disconnected before completion"
    ));
    assert!(CODEX.limit_words.contains(&"spend cap"));
    assert!(CODEX.auth_words.contains(&"was revoked"));
}

#[test]
fn a_passing_throttle_is_never_a_wall() {
    assert!(says(
        KIMI.limit_words,
        "Error: You exceeded your current token quota."
    ));
    assert!(says(
        KIMI.limit_words,
        "Insufficient balance: please recharge"
    ));
    assert!(says(
        GROK.limit_words,
        "You\u{2019}ve hit the rate limit for your plan"
    ));
    assert!(!says(
        GROK.limit_words,
        "the rate limit for this endpoint is documented below"
    ));
    assert!(!says(
        GROK.limit_words,
        "The service is busy. Wait a minute and send again."
    ));
}

/// EVERY GROK BUILD WALL THE SCREEN READS IS A LIMIT TO ITS POOL: each `grok.wall.*` anchor
/// aterm-phase holds to the installed binary, and the credit failures the binary's strings list.
/// CONTROL: the upsell's bare `Try Again` label, the `API error (status ` prefix alone and its
/// upgrade option say no limit.
#[test]
fn every_grok_wall_the_screen_reads_is_a_limit_to_its_pool() {
    let controls = ["grok.wall.try_again", "grok.wall.api_error"];
    let walls: Vec<_> = aterm_phase::anchors::GROK_ANCHORS
        .iter()
        .filter(|a| a.id.starts_with("grok.wall.") && !controls.contains(&a.id))
        .collect();
    assert!(!walls.is_empty(), "the filter keeps the wall anchors");
    for a in walls {
        assert!(says(GROK.limit_words, a.text), "{}: {}", a.id, a.text);
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
        assert!(says(GROK.limit_words, line), "{line}");
    }
    for id in controls {
        let text = aterm_phase::anchors::anchor(id);
        assert!(!says(GROK.limit_words, text), "CONTROL: {id}: {text}");
    }
    assert!(
        !says(GROK.limit_words, "Upgrade to a higher tier for more usage"),
        "CONTROL"
    );
}

/// EVERY GROK BUILD LOGIN-ENDED WORD IS ONE OF THE BUILD'S OWN LOGIN-ENDED TEXTS: each
/// `grok.auth.*` anchor (which aterm-phase's `vendor_drift` bench holds to the installed binary,
/// with a mutant of each as its negative control) says the login ended by a review's words (Grok
/// Build's row is in `ROWS`), and each word stands in one of those texts, so no word outlives the
/// build's strings.
/// NEGATIVE: grok 1.0.70's texts that end no login of a row's (a passing failure, an MCP
/// server's or Grove's own credential, the sign-in flow's own ending, a feature an API-key
/// session lacks, a team's deployment key, an admin's API-key policy, a configuration with no
/// sign-in, a model endpoint's own key, a 401 after a renewal) say no ended login to a review's
/// failure tail.
#[test]
fn every_grok_auth_word_is_one_of_the_builds_login_ended_texts() {
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
        assert!(says(GROK.auth_words, &line), "{}: {line}", a.id);
    }
    assert!(
        !says(GROK.auth_words, aterm_phase::anchors::anchor(prefix)),
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
        let lower = line.to_ascii_lowercase();
        assert!(
            !GROK.auth_words.iter().any(|w| lower.contains(w)),
            "a review reads {line:?} as an ended login"
        );
    }
}

/// A KIMI CODE ROW WHOSE MANAGED LOGIN LOST ITS TOKEN HAS ENDED: kimi's `-p` failure line for
/// it (measured on 2.3.1, `docs/measured/kimi-credentials-2026-10-24.md` §5, and 2.3.3,
/// `kimi-review-2026-10-27.md` §3) is an ended login by the pool's words and a review's.
/// NEGATIVE: the same sentence for an environment model with no key or a hand-written provider
/// with none, and a provider's 401 (all measured), are not.
#[test]
fn a_kimi_managed_login_with_no_token_has_ended_and_another_providers_has_not() {
    let ended =
        "error: failed to run prompt: provider managed:kimi-code has no credential configured";
    assert!(says(KIMI.auth_words, ended));
    for other in [
        "error: failed to run prompt: provider __kimi_env__ has no credential configured",
        "error: failed to run prompt: provider home has no credential configured",
        "error: failed to run prompt: provider.auth_error: 401 Invalid Authentication",
    ] {
        assert!(
            !KIMI.auth_words.iter().any(|w| other.contains(w)),
            "{other}"
        );
    }
}

/// The line kimi prints from the `kimi.auth.*` text `id`: a template's fixed part completed
/// with the managed login's names (`managed:kimi-code`, `kimi-code`).
fn kimi_auth_line(id: &str, text: &str) -> String {
    match id {
        "kimi.auth.no_credential" => format!("provider managed:kimi-code{text}"),
        "kimi.auth.no_token" => format!("No token for \"kimi-code{text}"),
        "kimi.auth.rejected" => format!("Stored token for \"kimi-code{text}"),
        "kimi.auth.no_refresh_token" => format!("Token for \"kimi-code{text}"),
        "kimi.auth.relogin_notice" => format!("{text}kimi-code"),
        _ => text.to_string(),
    }
}

/// EVERY KIMI CODE LOGIN-ENDED WORD IS ONE OF THE BUILD'S OWN LOGIN TEXTS: each `kimi.auth.*`
/// line 2.3.3 prints for a login it lacks or lost (aterm-phase's `vendor_drift` bench holds each
/// text to the installed binary) says the login ended by a review's words, and each word stands
/// in one of those lines, so no word outlives the build's strings.
/// CONTROL: a template's fixed part alone names no login, and the screen's `kimi.auth.llm_not_set`
/// (2.3.1's welcome state, not a `-p` failure) is left out.
#[test]
fn every_kimi_auth_word_is_one_of_the_builds_login_texts() {
    let lines: Vec<(&str, String)> = aterm_phase::anchors::KIMI_ANCHORS
        .iter()
        .filter(|a| a.id.starts_with("kimi.auth.") && a.id != "kimi.auth.llm_not_set")
        .map(|a| (a.id, kimi_auth_line(a.id, a.text)))
        .collect();
    assert!(lines.len() >= 10, "the filter keeps the login texts");
    for (id, line) in &lines {
        assert!(
            says(KIMI.auth_words, line),
            "a review reads {id} as no ended login: {line}"
        );
    }
    for word in KIMI.auth_words {
        assert!(
            lines.iter().any(|(_, l)| l.to_lowercase().contains(word)),
            "{word:?} is in no kimi.auth.* line"
        );
    }
    assert!(
        !says(
            KIMI.auth_words,
            aterm_phase::anchors::anchor("kimi.auth.no_credential")
        ),
        "CONTROL: no provider named"
    );
}

/// The phrases a pattern of kimi's quota classifier matches: each `|` alternative, with and
/// without each `(?:… )?` group.
fn kimi_pattern_phrases(pattern: &str) -> Vec<String> {
    let mut out = Vec::new();
    for alt in pattern.split('|') {
        let mut phrases = vec![String::new()];
        let mut rest = alt;
        while let Some(open) = rest.find("(?:") {
            let close = rest[open..].find(")?").expect("a closed optional group") + open;
            let (head, group) = (&rest[..open], &rest[open + 3..close]);
            phrases = phrases
                .into_iter()
                .flat_map(|p| [format!("{p}{head}{group}"), format!("{p}{head}")])
                .collect();
            rest = &rest[close + 2..];
        }
        out.extend(phrases.into_iter().map(|p| format!("{p}{rest}")));
    }
    out
}

/// KIMI CODE'S LIMIT WORDS ARE ITS OWN QUOTA CLASSIFIER'S: every phrase each `kimi.wall.*`
/// pattern or code matches (aterm-phase's `vendor_drift` bench holds them to the installed
/// binary) is a limit to a review, and every limit word is one of them; the measured 429 line is
/// a limit. CONTROL: a 429 that names no quota is not.
#[test]
fn kimis_limit_words_are_its_own_quota_classifiers() {
    let phrases: Vec<String> = aterm_phase::anchors::KIMI_ANCHORS
        .iter()
        .filter(|a| a.id.starts_with("kimi.wall."))
        .flat_map(|a| kimi_pattern_phrases(a.text))
        .collect();
    assert!(
        phrases.contains(&"exceeded your current token quota".to_string())
            && phrases.contains(&"account is in arrears".to_string())
            && phrases.contains(&"please recharge".to_string()),
        "{phrases:?}"
    );
    for phrase in &phrases {
        assert!(
            says(KIMI.limit_words, phrase),
            "a review reads {phrase:?} as no limit"
        );
    }
    for word in KIMI.limit_words {
        assert!(
            phrases.iter().any(|p| p.contains(word)),
            "{word:?} is no phrase of kimi's classifier"
        );
    }
    assert!(says(
        KIMI.limit_words,
        "error: failed to run prompt: provider.rate_limit: 429 You exceeded your current quota, \
         please check your plan and billing details."
    ));
    assert!(
        !says(
            KIMI.limit_words,
            "error: failed to run prompt: provider.rate_limit: 429 Too Many Requests"
        ),
        "CONTROL"
    );
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
        // Kimi Code's `-p` is at yolo with no flag, and refuses one beside it.
        assert!(
            !row.review.levels.yolo.is_empty() || row.review.only_at == Some(Level::Yolo),
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

/// The vault's refused set is read off the rows: a CLI's login file under its default home
/// (Muse Code's `~/.config/muse/.auth.json` too), the refresh aterm journals beside Codex's, and
/// a pool row's login under its root, without case. CONTROL: a CLI's settings beside its login,
/// and a pool root's own path, are no login.
#[test]
fn a_login_file_is_where_a_connector_row_keeps_its_login() {
    for (name, product) in [
        ("~/.config/muse/.auth.json", MUSE.product),
        ("~/.Codex/Auth.json", CODEX.product),
        ("~/.codex/auth.json.refreshed", CODEX.product),
        ("~/.kimi-code/credentials/kimi-code.json", KIMI.product),
        ("~/.aterm/logins/grok/work/auth.json", GROK.product),
    ] {
        assert_eq!(login_file(name), Some(product), "{name}");
    }
    for name in [
        "~/.codex/config.toml",
        "~/.config/muse/settings.json",
        "~/.aterm/logins/codex",
        "auth.json",
    ] {
        assert_eq!(login_file(name), None, "{name}");
    }
}

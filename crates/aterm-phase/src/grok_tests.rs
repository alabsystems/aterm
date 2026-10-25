// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fixtures::{
    REPLY_QUOTES_LOGIN_ENDED, REPLY_QUOTES_RATE_LIMIT, REPLY_QUOTES_UPSELL, SIGNED_OUT,
    WALL_AUTH_TOKEN_INVALID, WALL_CREDIT_LIMIT, WALL_PLAN_RATE_LIMIT, WALL_WEEKLY_LIMIT,
};
use super::*;
use crate::prompt::fixtures::{composer, rows, screen};
use crate::reader::{Program, identify, program_of, read};
use crate::wall::{Placement, WallKind};

#[test]
fn the_plan_rate_limit_card_is_a_limit_wall_and_prose_that_quotes_it_is_none() {
    let rows = screen(WALL_PLAN_RATE_LIMIT);
    let w = wall(&rows).expect("the limit's card is a wall");
    assert_eq!(w.kind, WallKind::UsageSession, "grok names no window");
    assert_eq!(w.message, "You've hit the rate limit for your plan.");
    assert_eq!(w.placement, Placement::ErrorRow);
    assert_eq!(w.reset, None);
    assert!(rows[w.row].contains("rate limit for your plan"));
    let r = read(Some("grok"), &rows, None);
    assert_eq!(r.program, Program::Grok);
    assert_eq!(
        r.wall.as_ref().map(|w| w.kind),
        Some(WallKind::UsageSession)
    );

    // Negative: a reply whose words quote the limit, even its whole sentence inside a line, is
    // no wall: the anchor is the row's opening, not a substring.
    let prose = screen(REPLY_QUOTES_RATE_LIMIT);
    assert!(
        prose
            .iter()
            .any(|r| r.contains("You've hit the rate limit for your plan."))
    );
    assert_eq!(wall(&prose), None);
    assert_eq!(read(Some("grok"), &prose, None).wall, None);
}

#[test]
fn every_limit_and_login_text_opens_a_wall_row_and_a_passing_throttle_does_not() {
    let card = |line: &str| {
        let mut rows = screen(WALL_PLAN_RATE_LIMIT);
        let at = rows
            .iter()
            .rposition(|r| r.contains("rate limit for your plan"))
            .expect("the card's body");
        rows[at] = format!("    {line}");
        rows
    };
    for (line, kind) in [
        (
            "You\u{2019}ve hit the rate limit for your plan. Upgrade your account or try again later.",
            WallKind::UsageSession,
        ),
        (
            "You\u{2019}ve hit your team\u{2019}s API rate limit. Ask a team admin to purchase more credits for higher limits, or try again later.",
            WallKind::UsageSession,
        ),
        (
            "API error (status 429): You\u{2019}ve hit your team\u{2019}s API rate limit. Ask a team admin to purchase more credits.",
            WallKind::UsageSession,
        ),
        (
            "\u{2717} API error (status 429): You\u{2019}ve hit the rate limit for your plan. Upgrade your account or try again later.",
            WallKind::UsageSession,
        ),
        (
            "You\u{2019}ve reached your free Grok Build usage limit for now. Get SuperGrok for much higher limits.",
            WallKind::UsageSession,
        ),
        (
            "Authentication required: your session has expired or your credentials were rejected.",
            WallKind::Auth,
        ),
        ("● Not logged in. Run `grok login`.", WallKind::Auth),
        ("You hit your free usage limit.", WallKind::UsageSession),
        ("You hit your weekly limit.", WallKind::UsageWeekly),
        (
            "You\u{2019}ve hit the credit limit for your plan.",
            WallKind::Spend,
        ),
        ("You\u{2019}ve hit your spending cap.", WallKind::Spend),
        (
            "Resubmit the last prompt once you have usage again",
            WallKind::UsageSession,
        ),
        (
            "› Try Again   Resubmit the last prompt once you have usage again",
            WallKind::UsageSession,
        ),
    ] {
        assert_eq!(wall(&card(line)).map(|w| w.kind), Some(kind), "{line}");
    }
    for line in [
        "The service is busy. Wait a minute and send again.",
        "a rate limit for your plan applies here",
        "it said: You've hit the rate limit for your plan.",
        "it said: You hit your weekly limit.",
        "it said: You\u{2019}ve hit your team\u{2019}s API rate limit.",
        "API error (status 500): Something went wrong on our side.",
        "API error (status ): You\u{2019}ve hit your team\u{2019}s API rate limit.",
        "API error (status 4x9): You\u{2019}ve hit the rate limit for your plan.",
        "it said: API error (status 429): You\u{2019}ve hit the rate limit for your plan.",
        "Try Again",
        "Try Again later, once the week resets",
    ] {
        assert_eq!(wall(&card(line)), None, "{line}");
    }
}

/// THE UPSELLS' TITLES NAME THEIR WINDOWS: the weekly limit is the week's wall, and the credit
/// limit and the spending cap are money's, which the billing period lifts. Each is read off the
/// title, not the `Try Again` row under it, and a reply that quotes the titles and the row is no
/// wall.
#[test]
fn the_upsells_titles_name_their_windows_and_prose_that_quotes_them_is_none() {
    for (fixture, kind, title) in [
        (
            WALL_WEEKLY_LIMIT,
            WallKind::UsageWeekly,
            "You hit your weekly limit.",
        ),
        (
            WALL_CREDIT_LIMIT,
            WallKind::Spend,
            "You\u{2019}ve hit the credit limit for your plan.",
        ),
    ] {
        let rows = screen(fixture);
        let w = wall(&rows).expect("the upsell is a wall");
        assert_eq!((w.kind, w.message.as_str()), (kind, title));
        assert_eq!(w.placement, Placement::ErrorRow);
        assert_eq!(w.reset, None, "grok names no reset time");
        assert!(rows[w.row].ends_with(title), "{:?}", rows[w.row]);
        let r = read(Some("grok"), &rows, None);
        assert_eq!(r.program, Program::Grok);
        assert_eq!(r.wall.map(|w| w.kind), Some(kind));
    }

    let prose = screen(REPLY_QUOTES_UPSELL);
    for quoted in [
        "You\u{2019}ve hit the credit limit for your plan.",
        "You hit your weekly limit.",
        "You\u{2019}ve hit your spending cap.",
        "Resubmit the last prompt once you have",
    ] {
        assert!(prose.iter().any(|r| r.contains(quoted)), "{quoted}");
    }
    assert_eq!(wall(&prose), None);
    assert_eq!(read(Some("grok"), &prose, None).wall, None);
}

/// A TALL UPSELL WHOSE TITLE IS OUT OF THE LIVE ZONE IS STILL A WALL, BY ITS `Try Again` ROW: a
/// usage limit that names no window. CONTROL: with its title in the zone, the title's window.
#[test]
fn a_try_again_row_under_a_title_out_of_the_live_zone_is_a_limit_naming_no_window() {
    let rows = screen(WALL_CREDIT_LIMIT);
    let title = rows
        .iter()
        .position(|r| r.contains("credit limit for your plan"))
        .expect("the title");
    assert_eq!(
        wall(&rows).map(|w| w.kind),
        Some(WallKind::Spend),
        "CONTROL"
    );
    let mut tall = rows.clone();
    tall.splice(
        title + 1..title + 1,
        (0..6).map(|i| format!("      option line {i}")),
    );
    let w = wall(&tall).expect("the Try Again row");
    assert_eq!(w.kind, WallKind::UsageSession);
    assert_eq!(
        w.message,
        "Resubmit the last prompt once you have usage again"
    );

    // The same with the label and its line on one row, the weekly upsell's shape.
    let weekly: Vec<String> = screen(WALL_WEEKLY_LIMIT)
        .into_iter()
        .filter(|r| !r.contains("weekly limit"))
        .collect();
    let w = wall(&weekly).expect("the Try Again row");
    assert_eq!(w.kind, WallKind::UsageSession);
    assert!(w.message.starts_with("Try Again"), "{}", w.message);
}

#[test]
fn a_limit_row_scrolled_above_the_live_zone_is_no_wall_of_the_last_turn() {
    let mut rows = screen(WALL_PLAN_RATE_LIMIT);
    let end = rows.len();
    rows.extend((0..30).map(|i| format!("  later reply row {i}")));
    assert!(
        wall(&rows).is_none(),
        "the card is {end} rows up, out of the live zone"
    );
}

#[test]
fn the_signed_out_screen_is_grok_s_own_and_its_sign_in_wall() {
    let rows = screen(SIGNED_OUT);
    assert!(is_grok_screen(&rows));
    let at = sign_in_row(&rows).expect("the approval row");
    let w = wall(&rows).expect("a sign-in wall");
    assert_eq!(
        (w.kind, w.row, w.placement),
        (WallKind::Auth, at, Placement::Banner)
    );
    assert_eq!(w.message, "Approve in your browser to finish signing in.");
    // By its process name, and by its screen alone.
    assert_eq!(program_of("grok"), Some(Program::Grok));
    assert_eq!(program_of("/opt/x/bin/grok"), Some(Program::Grok));
    assert_eq!(identify(None, &rows).program(), Program::Grok);
    let r = read(Some("grok"), &rows, None);
    assert_eq!(r.program, Program::Grok);
    assert!(!r.phase_authoritative, "no busy or idle screen is measured");

    // Negative: the same rows without its key row are not claimed by the sniff (the name still
    // reads them), and a shell screen stays the generic program's.
    let keyless: Vec<String> = rows
        .iter()
        .filter(|r| !r.contains("ctrl+q"))
        .cloned()
        .collect();
    assert!(!is_grok_screen(&keyless));
    let shell = screen("$ ls\nCargo.toml  src\n$ ");
    assert_eq!(identify(None, &shell).program(), Program::Generic);
    assert_eq!(identify(Some("zsh"), &rows).program(), Program::Generic);
}

/// A CLAUDE CODE SCREEN QUOTING GROK BUILD'S SIGN-IN SCREEN IS STILL CLAUDE CODE'S: its Bash
/// tool's output (`aterm ctl text` of a Grok tab) shows the approval line, `Waiting for
/// approval...` and the `ctrl+q  quit` key row above its own composer. CONTROL: those rows with no
/// Claude Code frame under them are Grok Build's by sight.
#[test]
fn a_claude_code_screen_quoting_groks_sign_in_screen_is_still_claude_codes() {
    let quote = rows(&[
        "⏺ Bash(aterm ctl @s-grok text | tail -12)",
        "  ⎿  ~/proj",
        "     Approve in your browser to finish signing in.",
        "     Make sure your browser shows this code.",
        "     Waiting for approval...",
        "     ctrl+q  quit",
        "",
    ]);
    assert!(is_grok_screen(&quote), "CONTROL: Grok Build's by sight");
    assert_eq!(identify(None, &quote).program(), Program::Grok, "CONTROL");
    let mut claude = quote;
    claude.extend(composer("  ? for shortcuts"));
    for program in [None, Some("node"), Some("ssh")] {
        assert_eq!(
            identify(program, &claude).program(),
            Program::Claude,
            "{program:?}"
        );
    }
}

/// A NOT-LOGGED-IN WALL IS GROK BUILD'S OWN SENTENCE, NOT THE BARE PHRASE OTHER CLIS OPEN WITH: a
/// reply quoting another CLI's `Not logged in. …` (netlify's) is no wall. CONTROL: Grok Build's
/// own `Not logged in. Run `grok login`.` is an ended login.
#[test]
fn a_reply_line_opening_with_another_clis_not_logged_in_is_no_grok_login_wall() {
    let card = |lines: &[&str]| {
        let mut rows = screen(WALL_PLAN_RATE_LIMIT);
        let at = rows
            .iter()
            .rposition(|r| r.contains("rate limit for your plan"))
            .expect("the card's body");
        rows.splice(at..=at, lines.iter().map(|l| (*l).to_string()));
        rows
    };
    let quoted = card(&[
        "  I ran `netlify status`; it printed:",
        "    Not logged in. Please log in to see site status.",
    ]);
    assert_eq!(wall(&quoted), None);
    assert_eq!(
        wall(&card(&["    Not logged in. Run `grok login`."])).map(|w| w.kind),
        Some(WallKind::Auth),
        "CONTROL"
    );
}

/// AN ENDED LOGIN'S ROW IS AN AUTH WALL, AND A REPLY QUOTING THE LOGIN TEXTS IS NONE: grok
/// 1.0.70's invalid-token row under a reply is [`WallKind::Auth`], read off the row's opening
/// past its mark. NEGATIVE: a reply whose prose quotes that text, the other login-ended texts and
/// the texts that end no login of the row's, inside its lines, is no wall.
#[test]
fn an_invalid_token_row_is_an_auth_wall_and_a_reply_quoting_login_texts_is_none() {
    let rows = screen(WALL_AUTH_TOKEN_INVALID);
    let w = wall(&rows).expect("the ended login's row is a wall");
    assert_eq!(w.kind, WallKind::Auth);
    assert_eq!(
        w.message,
        "Your auth token is invalid or expired. Run `grok login` to re-authenticate."
    );
    assert_eq!(w.placement, Placement::ErrorRow);
    assert!(
        rows[w.row].contains("invalid or expired"),
        "{:?}",
        rows[w.row]
    );
    let r = read(Some("grok"), &rows, None);
    assert_eq!(r.program, Program::Grok);
    assert_eq!(r.wall.map(|w| w.kind), Some(WallKind::Auth));

    let prose = screen(REPLY_QUOTES_LOGIN_ENDED);
    for quoted in [
        "Your auth token is invalid or expired.",
        "Token expired. Run",
        "Session expired. Run",
        "Your team sign-in was rejected.",
        "Not signed in. Run `grok login` to authenticate",
        "Your GROK_DEPLOYMENT_KEY is invalid or expired.",
    ] {
        assert!(prose.iter().any(|r| r.contains(quoted)), "{quoted}");
    }
    assert_eq!(wall(&prose), None);
    assert_eq!(read(Some("grok"), &prose, None).wall, None);
}

/// EVERY LOGIN-ENDED TEXT OPENS AN AUTH WALL ROW, AND NO TEXT OF A PASSING FAILURE OR OF ANOTHER
/// CREDENTIAL DOES: each line as grok 1.0.70 spells it (the named sign-in helper's with a name
/// in it), put where the plan rate limit's card body stands. CONTROL: the same texts quoted after
/// a word are none, and the passing, MCP, Grove, deployment-key and admin-policy texts that open
/// a row are none.
#[test]
fn every_login_ended_text_opens_an_auth_row_and_another_credentials_text_does_not() {
    let card = |line: &str| {
        let mut rows = screen(WALL_PLAN_RATE_LIMIT);
        let at = rows
            .iter()
            .rposition(|r| r.contains("rate limit for your plan"))
            .expect("the card's body");
        rows[at] = format!("    {line}");
        rows
    };
    let ended = [
        "Token expired. Run `grok login` to re-authenticate.",
        "Your auth token is invalid or expired. Run `grok login` to re-authenticate.",
        "Authentication rejected by server. Run `grok login` to re-authenticate.",
        "Auth recovery exhausted; re-authentication required. Run `grok login` to sign in with the required team.",
        "Session expired, re-authentication required",
        "Session expired. Run `grok login` to re-authenticate.",
        "Your session has expired. Run `grok login` to sign in again.",
        "Authentication required. Run `grok login` to re-authenticate.",
        "Your session expired and your sign-in helper could not renew it in the background. Run /login to sign in again.",
        "Your session expired and okta-helper could not renew it in the background. Run /login to sign in again.",
        "Authentication could not be refreshed. Run `grok login` to sign in again.",
        "Your team sign-in was rejected. It may have expired or lack access. Run `grok login` to sign in again.",
        "Not signed in. Run `grok login` to authenticate (or `grok login --device-code` if no browser is available).",
        "Not signed in. To authenticate without a browser, run:",
        "You are not authenticated.",
        "No cached auth token found",
        "Authentication failed. Run `grok login`, set XAI_API_KEY, or add api_key to ~/.grok/config.toml.",
        "No cached credentials found. Run `grok login` first.",
        "Headless mode requires a grok.com session. Run `grok login` to sign in, or use `grok agent stdio` for API-key access.",
        "preferred_method=oidc but no session is available. Run `grok login` to authenticate.",
        "Signed out: /feedback works, and /login signs in.",
        "Unauthorized (401): compaction failed \u{2014} re-authenticate with /login and retry.",
        "\u{2717} Your auth token is invalid or expired. Run `grok login` to re-authenticate.",
    ];
    for line in ended {
        assert_eq!(
            wall(&card(line)).map(|w| w.kind),
            Some(WallKind::Auth),
            "{line}"
        );
        let quoted = format!("it said: {line}");
        assert_eq!(wall(&card(&quoted)), None, "CONTROL: {quoted}");
    }
    for line in [
        "Authentication is temporarily unavailable. Run `grok login` if this persists.",
        "Token refresh failed transiently (network); skipping browser escalation",
        "Re-authenticated after 401; retrying request",
        "Access token expired or nearly expired, refreshing.",
        "Token refresh not possible, re-authorization required.",
        "Invalid token type: Token expired",
        "token expired and refresh is off; new session or enable GROVE_TOKEN_ROTATION=expected",
        "Grove Git credentials belong to the Grove daemon, not `grok login`.",
        "Skipping OAuth MCP in non-interactive mode (stored credentials unusable); re-authenticate in TUI",
        "Your GROK_DEPLOYMENT_KEY is invalid or expired. Please contact a team admin.",
        "The deployment key was rejected. Confirm that GROK_DEPLOYMENT_KEY is set correctly and hasn't expired.",
        "API-key auth is disabled by your administrator. Run `grok login` to authenticate.",
        "Authentication failed: the model endpoint rejected this build's credential. Check its key, then resend your message.",
        "Billing data requires auth with grok.com. Run `grok login` to authenticate.",
        "Device code expired. Run `grok login --device-auth` again.",
        "No login method available",
        "Log in or re-authenticate with your account",
        "Sign in again",
    ] {
        assert_eq!(wall(&card(line)), None, "{line}");
    }
}

/// A REPLY ROW OPENING WITH A SHORT LOGIN TEXT'S FIRST WORDS IS NO WALL, AND THE WHOLE TEXT IS
/// ONE, WRAPPED OR NOT: a row that opens with `Your session expired and`, `Your session has
/// expired.` or `You are not authenticated.` and goes on in a reply's own words, and the two
/// lowercase texts that follow another on grok's rows (a compaction failure's reason, voice's
/// `not signed in — …`), put where the plan rate limit's card body stands, are none. CONTROL:
/// the not-renewed text with the default or a named helper, and the session-expired text, each
/// whole on one row or wrapped onto the row under it, and the not-authenticated status line alone
/// on its row, are each [`WallKind::Auth`].
#[test]
fn a_reply_row_opening_with_a_short_login_texts_words_is_no_wall_and_the_whole_text_is_one() {
    let card = |lines: &[&str]| {
        let mut rows = screen(WALL_PLAN_RATE_LIMIT);
        let at = rows
            .iter()
            .rposition(|r| r.contains("rate limit for your plan"))
            .expect("the card's body");
        for (i, line) in lines.iter().enumerate() {
            rows[at + i] = format!("    {line}");
        }
        rows
    };
    for reply in [
        ["Your session expired and the CI job reran it.", ""],
        [
            "Your session expired and the build",
            "could not finish in the background.",
        ],
        ["Your session has expired. Sign in to the portal again.", ""],
        ["Your session has expired.", "Sign in to the portal again."],
        ["You are not authenticated. Ask an admin for access.", ""],
        [
            "authentication problem \u{2014} re-authenticate using /login and retry.",
            "",
        ],
        [
            "not signed in \u{2014} run `grok login`, set XAI_API_KEY, or set a model api_key/env_key",
            "",
        ],
    ] {
        assert_eq!(wall(&card(&reply)), None, "{reply:?}");
    }
    for ended in [
        &[
            "Your session expired and your sign-in helper could not renew it in the background. Run /login to sign in again.",
        ][..],
        &[
            "Your session expired and okta-helper could not renew it in the background. Run /login to sign in again.",
        ],
        &[
            "Your session expired and your sign-in helper could not",
            "renew it in the background. Run /login to sign in again.",
        ],
        &[
            "Your session expired and okta-helper",
            "could not renew it in the background. Run /login to sign in again.",
        ],
        &["Your session has expired. Run `grok login` to sign in again."],
        &[
            "Your session has expired. Run `grok login`",
            "to sign in again.",
        ],
        &["You are not authenticated."],
    ] {
        assert_eq!(
            wall(&card(ended)).map(|w| w.kind),
            Some(WallKind::Auth),
            "CONTROL: {ended:?}"
        );
    }
}

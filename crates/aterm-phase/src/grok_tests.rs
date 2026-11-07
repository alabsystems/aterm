// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fixtures::{
    BOX_SHELL, BUSY_DRAFT, DRAFT_MULTILINE_TYPED, IDLE_DRAFT, IDLE_EMPTY, PLAN_APPROVAL,
    REPLY_QUOTES_LOGIN_ENDED, REPLY_QUOTES_RATE_LIMIT, REPLY_QUOTES_UPSELL, RETRY_429_1_0_70,
    SIGNED_OUT, TURN_ENDED_EMPTY, UPSELL_FREE_1_0_70, UPSELL_WEEKLY_1_0_70, WALL_401_1_0_70,
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
    assert!(r.phase_authoritative, "its sign-in wall decides it");
    // CONTROL: the same screen with no `Waiting for approval...` under the approval row draws no
    // sign-in wall, and with no composer drawn nothing else decides its phase.
    let unwalled: Vec<String> = rows
        .iter()
        .filter(|r| !r.contains("Waiting for approval"))
        .cloned()
        .collect();
    assert_eq!(wall(&unwalled), None, "CONTROL: no sign-in wall");
    assert!(
        !read(Some("grok"), &unwalled, None).phase_authoritative,
        "CONTROL: nothing decides it"
    );

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

#[test]
fn the_composers_box_is_found_on_the_measured_frames_and_nowhere_else() {
    for (what, text) in [
        ("idle empty", IDLE_EMPTY),
        ("idle draft", IDLE_DRAFT),
        ("multiline draft", DRAFT_MULTILINE_TYPED),
        ("busy draft", BUSY_DRAFT),
        ("turn ended", TURN_ENDED_EMPTY),
        ("plan approval", PLAN_APPROVAL),
    ] {
        let rows = screen(text);
        let (top, caret, bottom) = composer_rows(&rows).expect(what);
        assert!(
            crate::text::trim_start(&rows[top]).starts_with('╭'),
            "{what}"
        );
        assert!(caret_row(&rows[caret]), "{what}");
        assert!(
            crate::text::trim_start(&rows[bottom]).starts_with('╰'),
            "{what}"
        );
        // The composer's box is the screen's last: no caret row stands under its bottom edge.
        assert!(!rows[bottom + 1..].iter().any(|r| caret_row(r)), "{what}");
    }
    // The multiline draft grew the box upward: one continuation row between caret and bottom.
    let rows = screen(DRAFT_MULTILINE_TYPED);
    let (_, caret, bottom) = composer_rows(&rows).expect("multiline");
    assert_eq!(bottom - caret, 2, "one continuation row");
    // NEGATIVE CONTROLS: an approval box standing with no composer drawn, and the sign-in
    // screen, hold no composer; the welcome card's own box (no caret row in it) is not the
    // composer under it, and a sent message's `❯` rows in the multiline frame's transcript
    // (no `│` before them) never make one.
    assert_eq!(composer_rows(&screen(BOX_SHELL)), None);
    assert_eq!(composer_rows(&screen(SIGNED_OUT)), None);
    let welcome = screen(IDLE_EMPTY);
    let (top, ..) = composer_rows(&welcome).expect("the composer under the card");
    assert!(
        welcome[..top]
            .iter()
            .any(|r| r.contains("Grok Build  1.0.70")),
        "the welcome card stands above the composer's top"
    );
}

/// ITS COMPOSER BOX CLAIMS NO OTHER PROGRAM'S SCREEN: over the whole measured fixture corpus,
/// no fixture of another program is read as a Grok Build screen (`is_grok_screen`), and every
/// Grok Build frame that draws the box identifies as Grok Build by screen alone, the signed-in
/// frames that drew nothing to tell them by before. So does each frame whose composer a
/// question card replaces (the two upsells, a question, three approvals), told by a radio
/// option on its heavy bar. NEGATIVE CONTROL: a Grok Build frame with no box, card or sign-in
/// screen (a hand-built wall, a quit) is claimed by nothing, an agy notice card on the same
/// bar is no question card, and the corpus holds such frames and over a thousand of the other
/// programs'.
#[test]
fn its_composer_box_claims_no_other_programs_screen() {
    let dir = aterm_tempfile::manifest_dir().join("src/fixtures");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("the fixtures")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".txt"))
        .collect();
    names.sort();
    assert!(names.len() > 1000, "{} fixtures", names.len());
    let (mut claimed, mut panel_only, mut unclaimed, mut others) = (0usize, 0usize, 0usize, 0usize);
    for name in &names {
        let rows = screen(&std::fs::read_to_string(dir.join(name)).expect("a fixture"));
        if name.starts_with("grok-") {
            if composer_rows(&rows).is_some() {
                claimed += 1;
                assert_eq!(
                    identify(None, &rows).program(),
                    Program::Grok,
                    "{name}: its box names it"
                );
            } else if panel_standing(&rows) {
                panel_only += 1;
                assert_eq!(
                    identify(None, &rows).program(),
                    Program::Grok,
                    "{name}: its question card names it"
                );
            } else if identify(None, &rows).program() != Program::Grok {
                unclaimed += 1;
            }
        } else {
            others += 1;
            assert!(!is_grok_screen(&rows), "{name}: no other program's screen");
        }
    }
    assert!(claimed > 30, "{claimed} box frames identify by screen");
    assert_eq!(
        panel_only, 8,
        "the upsell, question and approval cards with no composer"
    );
    assert!(unclaimed > 0, "frames with nothing to tell them by stand");
    assert!(others > 1000, "{others} other programs' frames");
    // A card named by screen alone reads its wall through the unnamed read too, where the
    // same frames read as a generic program's before (no composer to tell them by).
    assert_eq!(
        read(None, &screen(UPSELL_WEEKLY_1_0_70), None)
            .wall
            .map(|w| w.kind),
        Some(WallKind::UsageWeekly)
    );
}

#[test]
fn the_headers_token_counts_read_as_the_context_left() {
    for (what, text, left) in [
        ("idle draft", IDLE_DRAFT, 99),
        ("multiline draft", DRAFT_MULTILINE_TYPED, 95),
        ("busy draft", BUSY_DRAFT, 95),
        ("turn ended", TURN_ENDED_EMPTY, 95),
    ] {
        let rows = screen(text);
        assert_eq!(context_left(&rows), Some(left), "{what}");
        let r = read(Some("grok"), &rows, None);
        assert_eq!(r.context_left, Some(left), "{what}: the reading");
        // The watch does not take it: the header names no compaction.
        assert_eq!(Program::Grok.reader().context(&rows), None, "{what}");
    }
    // NEGATIVE CONTROLS: no header counts stand on the welcome card, the sign-in screen or a
    // hand-built wall, and the counts quoted deep in a transcript (the header row moved to
    // the foot of the screen) are no header.
    for (what, text) in [
        ("welcome card", IDLE_EMPTY),
        ("signed out", SIGNED_OUT),
        ("hand-built wall", WALL_PLAN_RATE_LIMIT),
    ] {
        assert_eq!(context_left(&screen(text)), None, "{what}");
    }
    let mut quoted = screen(IDLE_DRAFT);
    let header = quoted
        .iter()
        .position(|r| header_counts(r).is_some())
        .expect("the header row");
    let row = quoted.remove(header);
    quoted.push(row);
    assert_eq!(context_left(&quoted), None, "quoted counts are no header");
}

/// THE MEASURED WALLS ARE READ: a 401's drawn failure under the prompt at the screen's top
/// (a short transcript over a blank screen and its full-height scrollbar gutter, where a row
/// count from the foot never reached it) is an ended-login wall, and the upsells drawn in the
/// composer's place are their windows' walls. NEGATIVE CONTROLS: a 429's server text, which
/// no opener of the program's carries, is no wall; replies that quote the login-ended and
/// limit texts are none either; and a notice with a whole reply of worded rows under it has
/// been superseded.
#[test]
fn the_measured_walls_are_read_where_they_stand() {
    let rows = screen(WALL_401_1_0_70);
    let w = wall(&rows).expect("the 401 is a wall");
    assert_eq!(w.kind, WallKind::Auth);
    assert!(
        rows[w.row].contains("Authentication required:"),
        "{}",
        rows[w.row]
    );
    assert_eq!(
        read(Some("grok"), &rows, None).wall.map(|w| w.kind),
        Some(WallKind::Auth)
    );
    for (what, text, kind) in [
        (
            "free-usage upsell",
            UPSELL_FREE_1_0_70,
            WallKind::UsageSession,
        ),
        ("weekly upsell", UPSELL_WEEKLY_1_0_70, WallKind::UsageWeekly),
    ] {
        assert_eq!(wall(&screen(text)).map(|w| w.kind), Some(kind), "{what}");
    }
    // CONTROLS: the server's words after `Retry failed:` are no opener of the program's, and
    // prose quoting the wall texts opens no row in the zone.
    assert_eq!(wall(&screen(RETRY_429_1_0_70)), None, "CONTROL: the 429");
    assert_eq!(
        wall(&screen(REPLY_QUOTES_LOGIN_ENDED)),
        None,
        "CONTROL: quoted login-ended texts"
    );
    assert_eq!(
        wall(&screen(REPLY_QUOTES_RATE_LIMIT)),
        None,
        "CONTROL: quoted limit texts"
    );
    // CONTROL: the same failure with a reply of worded rows drawn under it is superseded.
    let mut superseded = screen(WALL_401_1_0_70);
    let at = superseded
        .iter()
        .position(|r| r.contains("Authentication required:"))
        .expect("the failure");
    for n in 0..LIVE_ROWS {
        superseded.insert(at + 2 + n, format!("the reply went on, line {n} of it"));
    }
    assert_eq!(wall(&superseded), None, "CONTROL: superseded");
}

/// Every Grok Build fixture by name, with the phase its rows decide and whether they decide it
/// (`phase`): the measured 1.0.70 frames, and the hand-built walls and replies. A new fixture
/// needs its row here.
const PHASES: &[(&str, Phase, bool)] = &[
    ("box-shell", Phase::Prompt, true),
    ("box-shell-approved", Phase::Idle, true),
    ("box-shell-ctrl-c", Phase::Idle, true),
    ("box-shell-denied", Phase::Idle, true),
    ("box-shell-escape", Phase::Prompt, true),
    ("box-shell-narrow", Phase::Prompt, true),
    ("box-write", Phase::Prompt, true),
    ("box-write-narrow", Phase::Prompt, true),
    ("box-write-approved", Phase::Idle, true),
    ("busy-ctrl-c-cancelled", Phase::Idle, true),
    ("busy-ctrl-c-queued", Phase::Busy, true),
    ("busy-ctrl-c-task", Phase::Idle, true),
    ("busy-draft", Phase::Busy, true),
    ("busy-escape", Phase::Busy, true),
    ("busy-queued", Phase::Busy, true),
    ("busy-streaming", Phase::Busy, true),
    ("busy-waiting", Phase::Busy, true),
    ("draft-ctrl-c", Phase::Idle, true),
    ("draft-multiline-typed", Phase::Idle, true),
    ("draft-paste-multiline", Phase::Idle, true),
    ("hand-built-reply-quotes-login-ended", Phase::Idle, false),
    ("hand-built-reply-quotes-rate-limit", Phase::Idle, false),
    ("hand-built-reply-quotes-upsell", Phase::Idle, false),
    ("hand-built-wall-auth-token-invalid", Phase::Idle, true),
    ("hand-built-wall-credit-limit", Phase::Idle, true),
    ("hand-built-wall-plan-rate-limit", Phase::Idle, true),
    ("hand-built-wall-weekly-limit", Phase::Idle, true),
    ("idle-background-task", Phase::Idle, true),
    ("idle-ctrl-c", Phase::Idle, true),
    ("idle-draft", Phase::Idle, true),
    ("idle-empty", Phase::Idle, true),
    ("plan-a-typed-approves", Phase::Idle, true),
    ("plan-approval", Phase::Prompt, true),
    ("plan-approval-empty", Phase::Prompt, true),
    ("plan-approved", Phase::Idle, true),
    ("plan-request-changes", Phase::Prompt, true),
    ("question", Phase::Prompt, true),
    ("question-answered", Phase::Idle, true),
    ("question-dismissed", Phase::Idle, true),
    ("quit-clean", Phase::Idle, false),
    ("quit-press-again", Phase::Idle, true),
    ("quit-sigkill", Phase::Idle, false),
    ("quit-sigterm", Phase::Idle, false),
    ("resume-id", Phase::Idle, true),
    ("resume-last", Phase::Idle, true),
    ("resume-unknown-id", Phase::Idle, false),
    ("signed-out", Phase::Idle, true),
    ("suggestion-accepted", Phase::Idle, true),
    ("turn-ended-empty", Phase::Idle, true),
    ("turn-ended-queued", Phase::Idle, true),
    ("turn-ended-suggestion", Phase::Idle, true),
    ("wall-401", Phase::Idle, true),
    ("wall-429", Phase::Idle, true),
    ("wall-429-retrying", Phase::Idle, true),
    ("wall-free-usage", Phase::Idle, true),
    ("wall-free-usage-dismissed", Phase::Idle, true),
    ("wall-weekly-limit", Phase::Idle, true),
];

/// The screen of the Grok Build fixture `name` (`grok-1.0.70-<name>.txt`).
fn frame(name: &str) -> Vec<String> {
    let path = aterm_tempfile::manifest_dir().join(format!("src/fixtures/grok-1.0.70-{name}.txt"));
    screen(&std::fs::read_to_string(&path).expect("the fixture"))
}

/// EVERY GROK BUILD FRAME READS THE PHASE ITS OWN ROWS DECIDE (measured on 1.0.70 at 144 columns,
/// and two approval cards at 70):
/// a running turn's frames (streaming, waiting for the first byte, a draft or a queued message
/// under it, Esc pressed, a queued message started by a cancel) are busy by the status row over
/// the composer's box ending `[stop]` and the key row under it offering `Ctrl+c:cancel`, and read
/// no wall; every frame a turn ended on (a reply, a cancel, a denial, a background task left
/// running, a resume, a 401's or a 429's failure) is idle; the sign-in screen and the upsells in
/// the composer's place are decided by their walls. The cards a turn raises in the box's place
/// (a permission, a question, also with the focus given to the scrollback) and a plan approval,
/// in either focus, are prompts, each read as its box, and no other frame reads a box. The
/// shell's frames after an exit, the hand-built replies that quote walls and the read with no
/// screen decide nothing. Display only: neither program is supervised.
#[test]
fn every_grok_frame_reads_the_phase_its_own_rows_decide() {
    let names: std::collections::BTreeSet<String> = crate::text::fixture_files()
        .into_iter()
        .filter_map(|(name, _)| {
            name.strip_prefix("grok-1.0.70-")
                .and_then(|n| n.strip_suffix(".txt"))
                .map(str::to_string)
        })
        .collect();
    let table: std::collections::BTreeSet<String> =
        PHASES.iter().map(|(n, ..)| (*n).to_string()).collect();
    assert_eq!(names, table, "every fixture has its decided phase");
    for (name, want, decided) in PHASES {
        let rows = frame(name);
        assert_eq!(phase(&rows), (want.clone(), *decided), "{name}");
        let r = read(Some("grok"), &rows, None);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (want.clone(), *decided),
            "{name}: the reading"
        );
        if *want == Phase::Busy {
            assert_eq!(r.wall, None, "{name}: a running turn reads no wall");
        }
        if *want == Phase::Prompt {
            assert!(r.prompt.is_some(), "{name}: the box");
            assert_eq!(r.wall, None, "{name}: a box reads no wall");
        } else {
            assert_eq!(prompt(&rows), None, "{name}: no box");
        }
    }
    assert!(
        PHASES
            .iter()
            .filter(|(_, p, d)| *p == Phase::Busy && *d)
            .count()
            >= 6,
        "the busy frames"
    );
    assert_eq!(
        read(Some("grok"), &frame("wall-weekly-limit"), None)
            .wall
            .map(|w| w.kind),
        Some(WallKind::UsageWeekly),
        "an upsell decides by its wall"
    );
    assert_eq!(
        phase(&[]),
        (Phase::Idle, false),
        "no screen decides nothing"
    );
    assert!(!Program::Grok.supervisable(), "display only (D23)");
}

/// THE BUSY ROWS COUNT ONLY WHERE THEY STAND AROUND THE BOX: a reply that quotes a running turn's
/// status row and its key row, high in an ended turn's transcript, is no busy turn.
/// NEGATIVE CONTROL for the mutant that reads `[stop]` or `Ctrl+c:cancel` off any row of the
/// screen.
#[test]
fn busy_rows_quoted_in_a_reply_are_no_running_turn() {
    let mut rows = frame("turn-ended-empty");
    let at = rows
        .iter()
        .position(|r| r.contains("Worked for"))
        .expect("the turn's end");
    rows.splice(
        at..at,
        [
            "     It drew this while the turn ran:".to_string(),
            "    ⠋ Responding… 8.9s        10.0s ⇣11.6k [stop]".to_string(),
            "  Shift+Tab:mode  │  Ctrl+c:cancel  │  Ctrl+x:shortcuts".to_string(),
            String::new(),
        ],
    );
    assert!(composer_rows(&rows).is_some(), "PRECONDITION: the box");
    assert_eq!(phase(&rows), (Phase::Idle, true));
    // CONTROL: the same status row right over the box, and that key row under it, are a turn's.
    let (top, _, bottom) = composer_rows(&rows).expect("the box");
    let mut running = rows.clone();
    running[bottom + 2] = "  Shift+Tab:mode  │  Ctrl+c:cancel  │  Ctrl+x:shortcuts".to_string();
    running[top - 2] = "    ⠋ Responding… 8.9s        10.0s ⇣11.6k [stop]".to_string();
    assert_eq!(phase(&running), (Phase::Busy, true), "CONTROL");
}

/// A BUSY ROW THE BUILD NO LONGER DRAWS DECIDES NOTHING, NEVER A WRONG PHASE: a running turn's
/// frame whose `[stop]`, whose `Ctrl+c:cancel`, or whose both are spelled otherwise reads no
/// phase as evidence (the spinner still opens its status row). NEGATIVE CONTROL for the mutants
/// that take either signal alone for a running turn or the absence of both for an idle one.
#[test]
fn a_busy_anchor_the_build_no_longer_draws_decides_nothing() {
    let busy = frame("busy-streaming");
    assert_eq!(phase(&busy), (Phase::Busy, true), "PRECONDITION");
    let respelled = |from: &[&str], to: &[&str]| -> Vec<String> {
        busy.iter()
            .map(|r| {
                from.iter()
                    .zip(to)
                    .fold(r.clone(), |row, (f, t)| row.replace(f, t))
            })
            .collect()
    };
    for (what, rows) in [
        ("[stop] respelled", respelled(&["[stop]"], &["[halt]"])),
        (
            "Ctrl+c:cancel respelled",
            respelled(&["Ctrl+c:cancel"], &["Ctrl+c:abort"]),
        ),
        (
            "both respelled",
            respelled(&["[stop]", "Ctrl+c:cancel"], &["[halt]", "Ctrl+c:abort"]),
        ),
    ] {
        assert!(!phase(&rows).1, "{what}");
        assert!(
            !read(Some("grok"), &rows, None).phase_authoritative,
            "{what}: the reading"
        );
    }
}

/// NO OTHER PROGRAM'S FRAME DECIDES A PHASE THROUGH GROK BUILD'S READING: over the whole fixture
/// corpus, every frame of another program reads nothing as evidence by `phase`. CONTROL: Grok
/// Build's own frames decide theirs (`every_grok_frame_reads_the_phase_its_own_rows_decide`).
#[test]
fn no_other_programs_frame_decides_a_grok_phase() {
    let mut others = 0usize;
    for (name, text) in crate::text::fixture_files() {
        if name.starts_with("grok-") || !name.ends_with(".txt") {
            continue;
        }
        others += 1;
        assert!(!phase(&screen(&text)).1, "{name}");
    }
    assert!(others > 1000, "{others} other programs' frames");
}

/// A CARD A RUNNING TURN RAISED STANDS OVER AN EARLIER TURN'S WALL ROW AND IS A PROMPT WITH NO
/// WALL: a question card whose live zone still holds a 401's failure from a turn before
/// (HAND-BUILT from the measured question and 401 frames) is no wall to publish, since the card's
/// turn runs (its status row ends `[stop]`) and waits on it. CONTROL: the upsell drawn in the
/// composer's place, with no turn running, decides by its wall. NEGATIVE CONTROL for the mutant
/// that lets any wall decide a screen with no composer.
#[test]
fn a_card_over_an_earlier_turns_wall_row_is_a_prompt_with_no_wall() {
    let failure = frame("wall-401");
    let failed: Vec<String> = failure
        .iter()
        .filter(|r| r.contains('┃'))
        .cloned()
        .collect();
    assert!(!failed.is_empty(), "PRECONDITION: the 401's rows");
    let mut rows = frame("question");
    let status = rows
        .iter()
        .position(|r| r.contains("Waiting on answers"))
        .expect("the card's status row");
    rows.splice(status - 1..status - 1, failed);
    assert_eq!(
        wall(&rows).map(|w| w.kind),
        Some(WallKind::Auth),
        "PRECONDITION: the wall reader sees the earlier failure"
    );
    assert_eq!(phase(&rows), (Phase::Prompt, true));
    let r = read(Some("grok"), &rows, None);
    assert_eq!(r.wall, None);
    assert_eq!(r.prompt.map(|p| p.kind), Some(PromptKind::Question));
    assert_eq!(
        Program::Grok.reader().wall(&rows),
        None,
        "nor by the reader's own wall"
    );
    assert_eq!(
        phase(&frame("wall-free-usage")),
        (Phase::Idle, true),
        "CONTROL"
    );
}

/// The options of `p` as `(number, label, focused)`.
fn options(p: &PromptV2) -> Vec<(Option<u8>, &str, bool)> {
    p.options
        .iter()
        .map(|o| (o.n, o.label.as_str(), o.focused))
        .collect()
}

/// EACH CARD READS AS THE BOX IT DRAWS (measured on 1.0.70 at 144 columns): the shell command's
/// approval is a Bash box whose command is its gutter rows (the read-only rule reads it), with
/// the focus left in the scrollback too; the file's approval is an Edit box naming its path; the
/// question's options are its radio rows up to each one's description, the write-in keyed by a
/// letter, none taken; every card is picked by its digits and offers no Esc that cancels it. The
/// plan approval, in either focus, is a plan's box that lists no option (its answers are letter
/// keys). No option carries a role: nothing answers these boxes.
#[test]
fn each_card_reads_as_the_box_it_draws() {
    let shell = prompt(&frame("box-shell")).expect("the shell card");
    assert_eq!(shell.kind, PromptKind::Bash);
    assert_eq!(shell.title, "Remove the lab file.");
    assert_eq!(shell.command, "rm lab-made.txt");
    assert_eq!(shell.command_rows, vec!["rm lab-made.txt"]);
    assert!(shell.gutter);
    assert_eq!(shell.readings(), vec!["rm lab-made.txt"]);
    assert_eq!(
        options(&shell),
        vec![
            (
                Some(1),
                "Yes, and don't ask again for anything (always-approve mode)",
                false
            ),
            (Some(2), "Always allow: rm lab-made.txt", false),
            (Some(3), "Yes, proceed", true),
            (Some(4), "No, reject (type to add feedback)", false),
            (Some(5), "Never allow: rm lab-made.txt", false),
        ]
    );
    assert_eq!(shell.select, Select::Digits);
    assert_eq!(shell.cancel, None);
    assert!(
        shell
            .footer
            .as_deref()
            .is_some_and(|f| f.starts_with("1/5:select"))
    );
    assert!(shell.options.iter().all(|o| o.role == Role::Other));

    let parked = prompt(&frame("box-shell-escape")).expect("the card, its focus parked");
    assert_eq!(
        (parked.kind, parked.command.as_str()),
        (PromptKind::Bash, "rm -r cache-dir")
    );
    assert_eq!(
        parked.focused().map(|o| o.n),
        Some(Some(3)),
        "the taken option stays"
    );

    // At 70 columns 1.0.70 cuts the card's key row short of `Esc:scrollback`, its title to one
    // row and an option's words at the edge: the card reads by its key row's opening hints.
    let narrow = prompt(&frame("box-shell-narrow")).expect("the shell card at 70 columns");
    assert_eq!(
        (narrow.kind, narrow.command.as_str()),
        (PromptKind::Bash, "rm -rf build-output")
    );
    assert_eq!(narrow.options.len(), 5);
    assert_eq!(narrow.focused().map(|o| o.n), Some(Some(3)));
    assert!(narrow.title.ends_with('…'), "{}", narrow.title);
    let narrow = prompt(&frame("box-write-narrow")).expect("the file card at 70 columns");
    assert_eq!(
        (narrow.kind, narrow.command.as_str(), narrow.path),
        (
            PromptKind::Edit,
            "/private/tmp/at.lab0…/notes-for-the-lab.txt",
            None
        ),
        "a path shortened in its middle names no file"
    );

    // An approval's option is read whole: what it names may hold two spaces of its own.
    let spaced: Vec<String> = frame("box-shell")
        .iter()
        .map(|r| r.replace("rm lab-made.txt", "rm 'a  b.txt'"))
        .collect();
    let spaced = prompt(&spaced).expect("the shell card");
    assert_eq!(spaced.command, "rm 'a  b.txt'");
    assert_eq!(
        options(&spaced)[1..],
        [
            (Some(2), "Always allow: rm 'a  b.txt'", false),
            (Some(3), "Yes, proceed", true),
            (Some(4), "No, reject (type to add feedback)", false),
            (Some(5), "Never allow: rm 'a  b.txt'", false),
        ]
    );

    let write = prompt(&frame("box-write")).expect("the file card");
    assert_eq!(write.kind, PromptKind::Edit);
    assert_eq!(
        write.path.as_deref(),
        Some("/private/tmp/at.lab0/proj/notes.txt")
    );
    assert_eq!(write.command, "/private/tmp/at.lab0/proj/notes.txt");
    assert!(write.command_rows.is_empty());
    assert_eq!(write.options.len(), 4);
    assert_eq!(write.focused().map(|o| o.label.as_str()), Some("Yes"));

    let question = prompt(&frame("question")).expect("the question card");
    assert_eq!(question.kind, PromptKind::Question);
    assert_eq!(question.title, "Which test runner should the lab use?");
    assert_eq!(question.description, question.title);
    assert_eq!(question.command, "");
    assert_eq!(
        options(&question),
        vec![
            (Some(1), "targo test", false),
            (Some(2), "lab selftest", false),
            (None, "Type your answer here", false),
        ]
    );

    for (name, title) in [
        ("plan-approval", "Waiting on plan approval"),
        (
            "plan-approval-empty",
            "No plan written: approve or request changes",
        ),
        ("plan-request-changes", "Waiting on plan approval"),
    ] {
        let plan = prompt(&frame(name)).expect(name);
        assert_eq!(plan.kind, PromptKind::PlanExit, "{name}");
        assert_eq!(plan.title, title, "{name}");
        assert!(plan.options.is_empty(), "{name}");
        assert!(
            plan.footer
                .as_deref()
                .is_some_and(|f| f.contains("a:approve")),
            "{name}"
        );
    }
}

/// A CARD IS A PROMPT ONLY WITH ITS TURN RUNNING AND ITS KEY ROW UNDER IT: the measured shell
/// card with its status row's `[stop]` taken away (as an upsell stands, its turn ended), with its
/// key row taken away, or with a reply's row between the bar and the key row, reads no box; the
/// plan approval's box whose key row offers no `a:approve` decides nothing. NEGATIVE CONTROL for
/// the mutants that read a box off the bar's radio rows alone, or off the plan approval's edge
/// alone.
#[test]
fn a_card_is_a_prompt_only_with_its_turn_running_and_its_key_row_under_it() {
    let card = frame("box-shell");
    assert_eq!(phase(&card), (Phase::Prompt, true), "PRECONDITION");
    let stopped: Vec<String> = card.iter().map(|r| r.replace("[stop]", "")).collect();
    assert_eq!(prompt(&stopped), None, "no turn running");
    assert_ne!(phase(&stopped).0, Phase::Prompt);
    let keys = card
        .iter()
        .rposition(|r| r.contains("Esc:scrollback"))
        .expect("the key row");
    let mut keyless = card.clone();
    keyless[keys] = String::new();
    assert_eq!(prompt(&keyless), None, "no key row");
    let mut parted = card.clone();
    parted.insert(keys, "     a reply's row".to_string());
    assert_eq!(
        prompt(&parted),
        None,
        "a row between the bar and the key row"
    );

    let plan = frame("plan-approval");
    let mut unoffered = plan.clone();
    let offer = unoffered
        .iter()
        .rposition(|r| r.contains("a:approve"))
        .expect("the plan's key row");
    unoffered[offer] = unoffered[offer].replace("a:approve", "v:select");
    assert_eq!(phase(&unoffered), (Phase::Idle, false));
    assert_eq!(prompt(&unoffered), None);
}

/// A RUNNING TURN READS NO WALL ITS LIVE ZONE STILL SHOWS FROM A TURN BEFORE: a 401's failure
/// rows over a running turn's reply (HAND-BUILT from the measured 401 and streaming frames) are
/// read by the wall reader, but the reading's wall is none and its phase busy. CONTROL: the same
/// failure under an ended turn is the reading's wall. NEGATIVE CONTROL for the mutant that reads
/// the wall whatever the phase.
#[test]
fn a_running_turn_reads_no_wall_from_a_turn_before() {
    let failed: Vec<String> = frame("wall-401")
        .into_iter()
        .filter(|r| r.contains('┃'))
        .collect();
    let mut rows = frame("busy-streaming");
    let reply = rows
        .iter()
        .position(|r| r.contains("tick 1 tick 2"))
        .expect("the reply");
    rows.splice(reply..reply, failed);
    assert_eq!(
        wall(&rows).map(|w| w.kind),
        Some(WallKind::Auth),
        "PRECONDITION: the wall reader sees it"
    );
    let r = read(Some("grok"), &rows, None);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Busy, true));
    assert_eq!(r.wall, None);
    assert_eq!(
        read(Some("grok"), &frame("wall-401"), None)
            .wall
            .map(|w| w.kind),
        Some(WallKind::Auth),
        "CONTROL"
    );
}

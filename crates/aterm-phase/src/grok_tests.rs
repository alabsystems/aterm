// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fixtures::{REPLY_QUOTES_RATE_LIMIT, SIGNED_OUT, WALL_PLAN_RATE_LIMIT};
use super::*;
use crate::prompt::fixtures::screen;
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
            "You\u{2019}ve reached your free Grok Build usage limit for now. Get SuperGrok for much higher limits.",
            WallKind::UsageSession,
        ),
        (
            "Authentication required: your session has expired or your credentials were rejected.",
            WallKind::Auth,
        ),
        ("● Not logged in. Run `grok login`.", WallKind::Auth),
    ] {
        assert_eq!(wall(&card(line)).map(|w| w.kind), Some(kind), "{line}");
    }
    for line in [
        "The service is busy. Wait a minute and send again.",
        "a rate limit for your plan applies here",
        "it said: You've hit the rate limit for your plan.",
    ] {
        assert_eq!(wall(&card(line)), None, "{line}");
    }
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

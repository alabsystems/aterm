// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fixtures::{CONCURRENT_LIMIT, DRAFT, IDLE, SIGNED_OUT_TURN, WALL_QUOTA};
use super::*;
use crate::prompt::fixtures::screen;
use crate::reader::{Program, identify, program_of, read};
use crate::wall::{Placement, WallKind};

#[test]
fn the_quota_exhausted_turn_is_a_wall_and_the_concurrency_throttle_is_none() {
    let rows = screen(WALL_QUOTA);
    let w = wall(&rows).expect("the quota 429 is a wall");
    assert_eq!(w.kind, WallKind::UsageSession, "kimi names no window");
    assert_eq!(w.placement, Placement::ErrorRow);
    assert!(
        w.message
            .starts_with("Error: You exceeded your current token quota")
    );
    let r = read(Some("kimi"), &rows, None);
    assert_eq!(r.program, Program::Kimi);
    assert_eq!(r.wall.map(|w| w.kind), Some(WallKind::UsageSession));

    // Negative: the 429 a request over the account's concurrency answers is a throttle Kimi Code
    // retries, no spent quota: no wall, so nothing a rotation would act on.
    let throttle = screen(CONCURRENT_LIMIT);
    assert!(
        throttle
            .iter()
            .any(|r| r.contains("concurrent request limit"))
    );
    assert_eq!(wall(&throttle), None);
    assert_eq!(read(Some("kimi"), &throttle, None).wall, None);
}

#[test]
fn a_balance_and_a_lost_login_are_walls_of_their_own_kinds() {
    let with = |line: &str| {
        let mut rows = screen(SIGNED_OUT_TURN);
        let at = rows
            .iter()
            .position(|r| r.starts_with("   Error: "))
            .expect("the error row");
        rows[at] = format!("   Error: {line}");
        rows
    };
    assert_eq!(
        wall(&screen(SIGNED_OUT_TURN)).map(|w| (w.kind, w.message)),
        Some((
            WallKind::Auth,
            "Error: LLM not set, send \"/login\" to login".to_string()
        )),
        "the measured signed-out turn"
    );
    for (line, kind) in [
        (
            "Insufficient balance, please recharge your account",
            WallKind::Spend,
        ),
        ("Your account is in arrears", WallKind::Spend),
        ("OAuth token refused: re-login required", WallKind::Auth),
        ("No token for \"kimi-code\"", WallKind::Auth),
    ] {
        assert_eq!(wall(&with(line)).map(|w| w.kind), Some(kind), "{line}");
    }
    // Negative: any other error, and the quota words anywhere but the last error row.
    assert_eq!(wall(&with("connection reset by peer")), None);
    let mut earlier = screen(WALL_QUOTA);
    let at = earlier
        .iter()
        .position(|r| r.starts_with("   Error: "))
        .expect("the error row");
    earlier.insert(at + 1, String::new());
    earlier.insert(at + 2, "   The next turn answered fine.".to_string());
    assert_eq!(
        wall(&earlier),
        None,
        "a later row stands under the old error"
    );
}

#[test]
fn the_composer_reads_its_draft_and_the_footer_its_context() {
    let idle = screen(IDLE);
    let (caret, lines) = composer(&idle).expect("the composer");
    assert!(idle[caret].contains("│ >"));
    assert_eq!(lines, vec![String::new()]);
    let draft = screen(DRAFT);
    assert_eq!(
        composer(&draft).map(|(_, l)| l),
        Some(vec!["hello".to_string()])
    );
    let r = read(Some("kimi"), &idle, None);
    assert_eq!(r.context_left, Some(100), "context: 0% used");
    assert!(!r.phase_authoritative, "a busy turn is not measured");
    assert_eq!(r.wall, None, "an idle composer is no wall");
}

#[test]
fn kimi_s_screens_are_its_own_by_name_and_by_sight() {
    assert_eq!(program_of("kimi"), Some(Program::Kimi));
    assert_eq!(program_of("kimi-code"), Some(Program::Kimi));
    for fixture in [IDLE, DRAFT, SIGNED_OUT_TURN, WALL_QUOTA] {
        let rows = screen(fixture);
        assert!(is_kimi_screen(&rows));
        assert_eq!(identify(None, &rows).program(), Program::Kimi);
        assert_eq!(identify(Some("node"), &rows).program(), Program::Kimi);
    }
    // Negative: a shell's own screen stays generic, and a pager showing Kimi Code's screen is
    // no Kimi Code.
    let shell = screen("$ ls\nCargo.toml  src\n$ ");
    assert!(!is_kimi_screen(&shell));
    assert_eq!(identify(None, &shell).program(), Program::Generic);
    assert_eq!(
        identify(Some("less"), &screen(IDLE)).program(),
        Program::Generic
    );
}

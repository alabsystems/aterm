// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The account's usage forms no reader reads, kept by the loop as `CAPTURED` records
// (`Session::screen_capture`, aterm-phase `usage_sighting`).

use super::reset::NoteSink;
use crate::supervise::journal::Capture;
use crate::supervise::run::capture_loop::CAPTURES_EACH;
use aterm_phase::prompt::fixtures::{FABLE_WARNING_2_1_301, screen};

/// The measured 2.1.301 Fable warning with `Fable limit` named `name` and its figure `pct`, still
/// drawn against the right edge, and a status line under it naming the login's address, as a
/// Claude Code screen.
fn warned(name: &str, pct: u32) -> Screen {
    let mut rows = screen(FABLE_WARNING_2_1_301);
    let at = rows
        .iter()
        .position(|r| r.contains("You've used 85% of your Fable limit"))
        .expect("the warning row");
    let width = rows[at].chars().count();
    let notice = rows[at]
        .trim()
        .replace("85%", &format!("{pct}%"))
        .replace("Fable limit", name);
    rows[at] = format!("{notice:>width$}");
    rows.push("  ana@example.com · Max 5x".to_string());
    Screen {
        rows,
        cols: Some(120),
        pane_rows: Some(50),
        seq: 9,
        ..Screen::default()
    }
}

fn claude_session(m: &mut Mock) -> Session<'_, Mock> {
    let mut s = session(m, Some("@s-1".to_string()));
    s.program = Some("claude".to_string());
    s
}

/// A loop keeps a `You've used …` warning no reader reads (the measured Fable warning, renamed
/// Opus) once, as one `CAPTURED … usage-warning-unread` record of the rows from the notice's row
/// to the screen's foot, the login's address masked; the same notice read again is kept never,
/// and past `CAPTURES_EACH` of them no more is. NEGATIVE CONTROL: the measured Fable warning,
/// which `model_week` reads, is kept never.
#[test]
fn usage_capture_keeps_an_unread_warning_once_and_bounded() {
    let opts = auto(60);
    let mut m = Mock::new(Vec::new());
    let mut s = claude_session(&mut m);
    let mut noted = NoteSink::default();
    let opus = warned("Opus limit", 85);
    s.screen_capture(&opus, &opts, &mut noted);
    s.screen_capture(&opus, &opts, &mut noted);
    assert_eq!(noted.0.len(), 1, "{:#?}", noted.0);
    let c = Capture::of_line(&noted.0[0]).expect("a capture");
    let top = aterm_phase::usage_sighting(&opus.rows, |_| true)
        .expect("a sighting")
        .top;
    assert_eq!(
        (c.what.as_str(), c.seq, c.first, c.grid, c.masked),
        ("usage-warning-unread", 9, top, Some((120, 50)), 1)
    );
    assert_eq!(
        c.rows,
        [
            opus.rows[top].trim_end().to_string(),
            "  xxx@xxxxxxx.xxx · Max 5x".to_string()
        ]
    );
    assert!(c.rows[0].contains("You've used 85% of your Opus limit"));
    assert!(!noted.0[0].contains("example"), "{}", noted.0[0]);
    assert!(
        !c.rows.iter().any(|r| r.contains("lab stub")),
        "the conversation above is cut: {:#?}",
        c.rows
    );
    for pct in 0..2 * u32::try_from(CAPTURES_EACH).expect("a small cap") {
        s.screen_capture(&warned("Opus limit", pct), &opts, &mut noted);
    }
    assert_eq!(noted.0.len(), CAPTURES_EACH, "{:#?}", noted.0);
    assert!(
        noted
            .0
            .iter()
            .filter_map(|line| Capture::of_line(line))
            .all(|c| c.what == "usage-warning-unread")
    );

    let mut m = Mock::new(Vec::new());
    let mut s = claude_session(&mut m);
    let mut none = NoteSink::default();
    s.screen_capture(&warned("Fable limit", 85), &opts, &mut none);
    assert!(none.0.is_empty(), "a read warning: {:#?}", none.0);
}

/// The usage-limit dialog names usage credits on its `Add funds to continue with usage credits`
/// row, and it is kept once as the reset program's (`reset-dialog`), never as a usage form: the
/// reset program's sighting comes first. NEGATIVE CONTROL: an agy session's screen of the same
/// rows is kept never.
#[test]
fn reset_capture_keeps_the_limit_dialog_as_the_reset_programs() {
    let opts = auto(60);
    let dialog = Screen {
        rows: screen(aterm_phase::prompt::fixtures::LIMIT_DIALOG_NO_RESET_ROWS_2_1_301),
        ..Screen::default()
    };
    assert!(
        dialog
            .rows
            .iter()
            .any(|r| r.contains("continue with usage credits"))
    );
    let mut m = Mock::new(Vec::new());
    let mut s = claude_session(&mut m);
    let mut noted = NoteSink::default();
    s.screen_capture(&dialog, &opts, &mut noted);
    s.screen_capture(&dialog, &opts, &mut noted);
    let whats: Vec<String> = noted
        .0
        .iter()
        .filter_map(|line| Capture::of_line(line))
        .map(|c| c.what)
        .collect();
    assert_eq!(whats, ["reset-dialog"], "{:#?}", noted.0);

    let mut m = Mock::new(Vec::new());
    let mut agy = session(&mut m, Some("@s-2".to_string()));
    agy.program = Some("agy".to_string());
    let mut none = NoteSink::default();
    agy.screen_capture(&dialog, &opts, &mut none);
    agy.screen_capture(&warned("Opus limit", 85), &opts, &mut none);
    assert!(none.0.is_empty(), "{:#?}", none.0);
}

/// A screen that shows both the reset program and an unread usage form, a `/rate-limit-options`
/// answer the newest thing said and an Opus warning in the footer, is kept once as the reset
/// program's (`reset-answer-spent`), the warning's rows among its own: the reset program's
/// sighting comes first. NEGATIVE CONTROL: the usage sighting alone takes the same screen as the
/// unread warning, so the order is what keeps it the reset program's.
#[test]
fn a_screen_both_sightings_take_is_kept_as_the_reset_programs() {
    let opts = auto(60);
    let mut rows = screen(aterm_phase::reset::fixtures::SPENT_SESSION_2_1_312);
    let width = rows
        .iter()
        .rfind(|r| aterm_phase::phase::is_rule(r))
        .expect("the composer's rule")
        .chars()
        .count();
    let notice =
        "You've used 85% of your Opus limit · resets Oct 19 at 11:36pm (America/Los_Angeles)";
    rows.push(format!("{notice:>width$}"));
    assert!(
        aterm_phase::reset_sighting(&rows).is_some_and(|seen| matches!(
            seen.sight,
            aterm_phase::ResetSight::Answer(aterm_phase::ResetLine::Spent { .. })
        ))
    );
    assert_eq!(
        aterm_phase::usage_sighting(&rows, |_| true).map(|seen| seen.what),
        Some("usage-warning-unread"),
        "the control"
    );
    let both = Screen {
        rows,
        ..Screen::default()
    };
    let mut m = Mock::new(Vec::new());
    let mut s = claude_session(&mut m);
    let mut noted = NoteSink::default();
    s.screen_capture(&both, &opts, &mut noted);
    let kept: Vec<Capture> = noted
        .0
        .iter()
        .filter_map(|line| Capture::of_line(line))
        .collect();
    assert_eq!(
        kept.iter().map(|c| c.what.as_str()).collect::<Vec<_>>(),
        ["reset-answer-spent"],
        "{:#?}",
        noted.0
    );
    assert!(
        kept[0]
            .rows
            .iter()
            .any(|r| r.contains("of your Opus limit"))
    );
}

/// A capture keeps the composer's frame and the rows under it, but not what the person typed and
/// has not sent: an unread Opus wall over a composer holding a draft is kept with the draft's
/// letters and digits masked, its `❯` and its width kept, the wall's row and the mode row under
/// the composer as drawn. NEGATIVE CONTROL: the same screen with an empty composer keeps the
/// composer row as drawn.
#[test]
fn a_capture_masks_the_composers_draft() {
    use aterm_phase::prompt::fixtures::WALL_FABLE_RESET_2_1_301;
    let opts = auto(60);
    let walled = |draft: &str| {
        let mut rows = screen(WALL_FABLE_RESET_2_1_301);
        let wall = rows
            .iter()
            .position(|r| r.contains("You've hit your Fable limit"))
            .expect("the wall row");
        rows[wall] = rows[wall].replace("Fable limit", "Opus limit");
        let composer = aterm_phase::phase::composer_index(&rows).expect("the composer row");
        rows[composer] = format!("❯{draft}");
        Screen {
            rows,
            ..Screen::default()
        }
    };
    let keep = |screen: &Screen| {
        let mut m = Mock::new(Vec::new());
        let mut s = claude_session(&mut m);
        let mut noted = NoteSink::default();
        s.screen_capture(screen, &opts, &mut noted);
        assert_eq!(noted.0.len(), 1, "{:#?}", noted.0);
        Capture::of_line(&noted.0[0]).expect("a capture")
    };
    let c = keep(&walled(" paste: hunter2 is the password"));
    assert_eq!(c.what, "usage-wall-unread");
    assert!(c.rows[0].contains("You've hit your Opus limit · resets 1:36am"));
    assert!(
        c.rows
            .iter()
            .any(|r| r == "❯ xxxxx: xxxxxxx xx xxx xxxxxxxx"),
        "{:#?}",
        c.rows
    );
    assert!(
        !c.rows.iter().any(|r| r.contains("hunter2")),
        "{:#?}",
        c.rows
    );
    assert!(
        c.rows
            .iter()
            .any(|r| r.contains("⏵⏵ auto mode on (shift+tab to cycle)"))
    );
    let empty = keep(&walled(""));
    assert!(
        empty.rows.iter().any(|r| r == "❯"),
        "the control: {:#?}",
        empty.rows
    );
}

/// The Fable consent box the approval policy's own tests draw
/// (`a_credits_consent_is_taken_only_under_usage_credits`), over the reset confirmation's head
/// rows, the focus on `Switch to Opus 5.5 and continue` as it opens, or moved down onto
/// `Continue with Fable 5.1` (`on_consent`).
fn consent_rows(on_consent: bool) -> Vec<String> {
    let (switch, consent) = if on_consent {
        ("  ", "❯ ")
    } else {
        ("❯ ", "  ")
    };
    let mut rows: Vec<String> = screen(aterm_phase::reset::fixtures::CONFIRM)[..3].to_vec();
    rows.extend([
        "   You've reached your Fable limit".to_string(),
        String::new(),
        "   You've used your included Fable usage for this week. Continuing on Fable 5.1 uses"
            .to_string(),
        "   usage credits — you have $12.00 in credits.".to_string(),
        "   https://support.claude.com/en/articles/12429409-extra-usage-for-paid-claude-plans"
            .to_string(),
        String::new(),
        format!("   {switch}Switch to Opus 5.5 and continue"),
        format!("   {consent}Continue with Fable 5.1"),
        String::new(),
        "   Enter to confirm · Esc to cancel".to_string(),
    ]);
    rows
}

/// A consent box is kept before it is answered: the look keeps its `CAPTURED … usage-consent`
/// record before it decides the box, so the record is journaled ahead of the answer's `APPROVED`
/// line and its keys, and the box the answer takes away is still measured. With usage credits
/// off, the default, `credits-decline@v1` cancels it with one Esc; with them on (`[harness]
/// usage_credits`) `credits-consent@v1` moves the focus down onto `Continue with Fable 5.1`, sees
/// it land there, and enters it. The script draws the box again with the focus moved, as Claude
/// Code redraws it under the arrow: the loop enters only a focus it has read land, so a script
/// with no such screen leaves the arrow unconfirmed and the box escalated. NEGATIVE CONTROL: each
/// switch's answer is never the other's: no Esc and no decline with credits on, no consent and no
/// Enter with them off.
#[test]
fn a_credits_consent_box_is_captured_before_its_answer() {
    for credits in [false, true] {
        let (dir, path) = journal_file(if credits {
            "usage-consent-on"
        } else {
            "usage-consent-off"
        });
        let mut m = if credits {
            let mut m = fenced(vec![consent_rows(false), consent_rows(true), idle_screen()]);
            m.key_gates = vec![(0, 0), (1, 1)];
            m
        } else {
            let mut m = fenced(vec![consent_rows(false), idle_screen()]);
            m.key_releases = Some(0);
            m
        };
        let mut opts = auto(30);
        opts.policy.approve = Approve::All;
        opts.policy.usage_credits = credits;
        opts.journal = Some(path.clone());
        let mut s = claude_session(&mut m);
        let _ = s.supervise(&opts);
        drop(s);
        let lines: Vec<String> = journal_records(&path)
            .0
            .into_iter()
            .map(|r| r.line)
            .collect();
        let captured: Vec<(usize, Capture)> = lines
            .iter()
            .enumerate()
            .filter_map(|(i, line)| Some((i, Capture::of_line(line)?)))
            .collect();
        assert_eq!(captured.len(), 1, "{lines:#?}");
        let (at, c) = &captured[0];
        assert_eq!(c.what, "usage-consent", "{lines:#?}");
        assert!(c.rows[0].starts_with('▔'), "from the rule: {:#?}", c.rows);
        assert!(
            !c.rows.iter().any(|r| r.contains("Suites are running")),
            "{:#?}",
            c.rows
        );
        let (rule, other) = if credits {
            ("rule=credits-consent@v1", "rule=credits-decline@v1")
        } else {
            ("rule=credits-decline@v1", "rule=credits-consent@v1")
        };
        let approved = lines
            .iter()
            .position(|l| l.starts_with("APPROVED "))
            .unwrap_or_else(|| panic!("{rule}: {lines:#?}"));
        assert!(*at < approved, "{lines:#?}");
        assert!(lines.iter().any(|l| l.contains(rule)), "{lines:#?}");
        assert!(!lines.iter().any(|l| l.contains(other)), "{lines:#?}");
        let presses = m.presses();
        let keys: Vec<&str> = presses
            .iter()
            .map(|p| p.rsplit(' ').next().unwrap_or_default())
            .collect();
        if credits {
            assert_eq!(keys, ["down", "enter"], "{:#?}", m.requests);
            assert!(
                presses[1].contains("Continue\\x20with\\x20Fable\\x205\\.1"),
                "entered on the consent's own row: {presses:#?}"
            );
        } else {
            assert_eq!(keys, ["esc"], "{:#?}", m.requests);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

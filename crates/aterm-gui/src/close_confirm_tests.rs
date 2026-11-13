// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn text_of(row: &[RenderCell]) -> String {
    row.iter().map(|cell| cell.ch).collect()
}

fn quit_prompt() -> ConfirmPrompt {
    crate::quit_safety::confirm_prompt(true, true, true).expect("a busy quit always prompts")
}

fn close_prompt() -> ConfirmPrompt {
    crate::quit_safety::confirm_prompt(false, true, true).expect("a busy close prompts")
}

/// The banner shows what the native dialog showed: title, body and both answers, the
/// affirmative key naming the prompt's verb.
#[test]
fn banner_shows_question_body_and_verb_named_keys() {
    let rows = banner_rows(&quit_prompt(), 100, BANNER_ROWS, Theme::default());
    assert_eq!(rows.len(), BANNER_ROWS);
    for row in &rows {
        assert_eq!(row.len(), 100, "every row must be exactly cols wide");
    }
    let row0 = text_of(&rows[0]);
    assert!(row0.contains(quit_prompt().title), "{row0}");
    assert!(row0.contains("Enter quits"), "{row0}");
    assert!(row0.contains("Esc cancels"), "{row0}");
    let row1 = text_of(&rows[1]);
    assert!(row1.contains(quit_prompt().body), "{row1}");

    let rows = banner_rows(&close_prompt(), 100, BANNER_ROWS, Theme::default());
    let row0 = text_of(&rows[0]);
    assert!(row0.contains("Enter closes"), "{row0}");
    assert!(!row0.contains("quits"), "{row0}");
}

/// The seam is the band's top edge and runs the whole width in one tone (the
/// `paste_banner` contract); the body row sits inside the band.
#[test]
fn the_seam_runs_unbroken_across_the_question_row() {
    let rows = banner_rows(&quit_prompt(), 100, BANNER_ROWS, Theme::default());
    assert!(rows[0].iter().all(|cell| cell.overline));
    let seams: std::collections::BTreeSet<Option<[u8; 3]>> =
        rows[0].iter().map(|cell| cell.overline_color).collect();
    assert_eq!(seams.len(), 1, "{seams:?}");
    assert!(rows[1].iter().all(|cell| !cell.overline));
}

/// Degenerate geometry must not panic: zero columns, one row, a window
/// narrower than the title.
#[test]
fn degenerate_geometry_never_panics() {
    for cols in [0_usize, 1, 3, 8, 20, 200] {
        for panel_rows in [0_usize, 1, BANNER_ROWS] {
            let rows = banner_rows(&quit_prompt(), cols, panel_rows, Theme::default());
            assert_eq!(rows.len(), panel_rows);
            for row in &rows {
                assert_eq!(row.len(), cols);
            }
        }
    }
}

/// A narrow window still shows the answer keys: the band swallows every other key, so
/// without them there is no visible way out.
#[test]
fn a_narrow_band_keeps_the_answer_keys() {
    let multi = crate::quit_safety::ConfirmPrompt {
        title: "Close all tabs?",
        body: "A running process ends.",
        proceed: "Close",
    };
    let pending = PendingClose::new(
        crate::WindowId(0),
        multi,
        CloseReplay::Window(crate::WindowId(0)),
        ExitActor::Human,
    );
    assert_eq!(pending.wanted_rows(100), BANNER_ROWS);
    let row0 = text_of(&banner_rows(&multi, 40, BANNER_ROWS, Theme::default())[0]);
    assert!(row0.contains("Enter \u{00b7} Esc"), "{row0:?}");
    assert_eq!(pending.wanted_rows(24), BANNER_ROWS + 1);
    let rows = banner_rows(&multi, 24, BANNER_ROWS + 1, Theme::default());
    assert!(
        text_of(&rows[1]).contains("Enter"),
        "{:?}",
        text_of(&rows[1])
    );
    assert!(
        text_of(&rows[2]).contains("A running"),
        "{:?}",
        text_of(&rows[2])
    );
    let row0 = text_of(&banner_rows(&multi, 24, 1, Theme::default())[0]);
    assert!(row0.contains("Enter") && row0.contains("Esc"), "{row0:?}");
}

/// The wire kind follows what the answer does: a last-tab close of the last window is a
/// quit on Windows, and its prompt says so.
#[test]
fn kind_follows_the_prompt_verb() {
    assert_eq!(kind(&quit_prompt()), "quit");
    assert_eq!(kind(&close_prompt()), "close-window");
    let multi = crate::quit_safety::ConfirmPrompt {
        title: "Close all tabs?",
        body: "…",
        proceed: "Close",
    };
    assert_eq!(kind(&multi), "close-window");
}

/// The `controls front` line is the agent's whole picture: that a confirm is
/// pending, over which window, what it asks, who asked, and the two wire
/// spellings that answer it — the `confirm` verb's, never a keystroke, so the
/// printed answer reaches the question whichever window is in front.
#[test]
fn controls_line_reports_the_question_and_how_to_answer_it() {
    let pending = PendingClose::new(
        crate::WindowId(3),
        quit_prompt(),
        CloseReplay::Window(crate::WindowId(3)),
        ExitActor::Ctl,
    );
    assert!(pending.wire_may_answer());
    let line = pending.controls_line();
    assert!(
        line.starts_with("confirm open=true kind=quit window=3 by=ctl "),
        "{line}"
    );
    assert!(
        line.contains(&format!("title=\"{}\"", quit_prompt().title)),
        "{line}"
    );
    assert!(line.contains("proceed=Quit"), "{line}");
    assert!(line.contains("answer=\"confirm yes\""), "{line}");
    assert!(line.contains("cancel=\"confirm no\""), "{line}");
    assert!(!line.contains("key "), "a keystroke never answers: {line}");
    assert!(!line.contains('\n'), "one line: {line:?}");
    assert_eq!(CONTROLS_CLOSED, "confirm open=false");
    assert_eq!(
        pending.wire_pending(),
        "confirm pending kind=quit window=3; `confirm yes|no` answers it"
    );
    assert_eq!(
        pending.wire_refusal(),
        "a quit is waiting for an answer in the window; `confirm yes|no` answers it"
    );
    assert_eq!(pending.wire_answered(true), "answered=yes kind=quit");
    assert_eq!(pending.wire_answered(false), "answered=no kind=quit");
}

/// The `confirm` grammar: exactly `yes` or `no`. A guess — a bare `confirm`,
/// `confirm y`, a trailing word — is the usage line, never an answer: `yes`
/// quits, so nothing but `yes` may mean it.
#[test]
fn the_confirm_verb_takes_exactly_yes_or_no() {
    assert_eq!(parse_wire_answer("yes"), Ok(true));
    assert_eq!(parse_wire_answer("  no "), Ok(false));
    for bad in ["", "y", "YES", "enter", "yes now", "no yes", "maybe"] {
        assert_eq!(
            parse_wire_answer(bad),
            Err("usage: confirm yes|no".to_string()),
            "{bad:?}"
        );
    }
}

/// A question a person asked is theirs to answer: the wire may not, the report says who asked
/// and that no wire verb answers it, and a gesture with no close scope of its own (the Quit
/// menu) counts as the person's. A control client's sid asks on the wire like `ctl` does.
#[test]
fn a_question_a_person_asked_is_not_answered_from_the_wire() {
    for asked_by in [ExitActor::Human, ExitActor::Unknown] {
        let pending = PendingClose::new(
            crate::WindowId(0),
            close_prompt(),
            CloseReplay::Window(crate::WindowId(0)),
            asked_by,
        );
        assert_eq!(pending.asked_by, ExitActor::Human);
        assert!(!pending.wire_may_answer());
        let line = pending.controls_line();
        assert!(
            line.starts_with("confirm open=true kind=close-window window=0 by=human "),
            "{line}"
        );
        assert!(
            line.ends_with("answer=\"-\" cancel=\"-\""),
            "no wire verb answers it: {line}"
        );
        assert_eq!(
            pending.wire_refusal(),
            "a window close is waiting for the person at the window"
        );
    }
    let by_sid = PendingClose::new(
        crate::WindowId(0),
        close_prompt(),
        CloseReplay::Window(crate::WindowId(0)),
        ExitActor::Sid("s-agent".into()),
    );
    assert!(by_sid.wire_may_answer());
    assert!(
        by_sid.controls_line().contains(" by=s-agent "),
        "{}",
        by_sid.controls_line()
    );
    let by_bridge = PendingClose::new(
        crate::WindowId(0),
        close_prompt(),
        CloseReplay::Window(crate::WindowId(0)),
        ExitActor::Bridge,
    );
    assert!(by_bridge.wire_may_answer());
}

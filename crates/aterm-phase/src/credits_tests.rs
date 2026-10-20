// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::prompt::fixtures::{
    CREDITS_IN_USE_2_1_300, CREDITS_IN_USE_80_2_1_300, CREDITS_IN_USE_FULLSCREEN_2_1_300,
    CREDITS_STARTED_2_1_300, CREDITS_STARTED_80_2_1_300, CREDITS_STARTED_FULLSCREEN_2_1_300,
    USAGE_WARNING_WEEKLY_2_1_300, WALL_WATCHDOG_SESSION_STATUSLINE_2_1_300, screen,
};

/// `rows` with row `i` replaced by `row`.
fn with_row(mut rows: Vec<String>, i: usize, row: &str) -> Vec<String> {
    rows[i] = row.to_string();
    rows
}

/// The index of the first row that holds `text`.
fn row_of(rows: &[String], text: &str) -> usize {
    rows.iter()
        .position(|r| r.contains(text))
        .unwrap_or_else(|| panic!("no row holds {text:?}: {rows:#?}"))
}

/// Every measured credits screen (2.1.300: both renderers, 110 and 80
/// columns) reads as credits in use; the notice they start on names the
/// five-hour window and its reset, cut at 80 columns or not.
#[test]
fn the_measured_credits_notices_read_as_credits_in_use() {
    let started = |reset: &str| Credits {
        started: Some(Started {
            window: Some(Window::Session),
            reset: Some(reset.to_string()),
        }),
        model: None,
    };
    assert_eq!(
        in_use(&screen(CREDITS_STARTED_2_1_300)),
        Some(started("9:43am (America/Los_Angeles)"))
    );
    assert_eq!(
        in_use(&screen(CREDITS_STARTED_FULLSCREEN_2_1_300)),
        Some(started("9:44am (America/Los_Angeles)"))
    );
    let cut = in_use(&screen(CREDITS_STARTED_80_2_1_300)).expect("credits at 80 columns");
    let begun = cut.started.expect("the started notice");
    assert_eq!(begun.window, Some(Window::Session));
    assert!(
        begun
            .reset
            .as_deref()
            .is_some_and(|r| r.starts_with("9:44am")),
        "{begun:?}"
    );
    for lasting in [
        CREDITS_IN_USE_2_1_300,
        CREDITS_IN_USE_80_2_1_300,
        CREDITS_IN_USE_FULLSCREEN_2_1_300,
    ] {
        assert_eq!(
            in_use(&screen(lasting)),
            Some(Credits {
                started: None,
                model: None
            }),
            "{lasting}"
        );
    }
}

/// The notice's PER-MODEL form (READ in the 2.1.304 bundle, `wbo`: `Now using
/// usage credits for ${model} · Your ${model} limit resets ${reset}`, drawn
/// where the window is a model's) is the one credits started on, its reset
/// the model's, and names the model, in place of the measured started
/// notice's row; its head naming no reset names the model and no start.
/// NEGATIVE CONTROL: the bare lasting notice names neither.
#[test]
fn the_per_model_notice_names_its_model_and_its_reset() {
    let rows = screen(CREDITS_STARTED_2_1_300);
    let at = row_of(&rows, "You're now using usage credits");
    let notice = |text: &str| with_row(rows.clone(), at, &format!("{text:>108}"));
    assert_eq!(
        in_use(&notice(
            "Now using usage credits for Opus · Your Opus limit resets 4pm"
        )),
        Some(Credits {
            started: Some(Started {
                window: None,
                reset: Some("4pm".to_string()),
            }),
            model: Some("Opus".to_string()),
        })
    );
    assert_eq!(
        in_use(&notice("Now using usage credits for Opus")),
        Some(Credits {
            started: None,
            model: Some("Opus".to_string()),
        })
    );
    assert_eq!(
        in_use(&notice("Now using usage credits")),
        Some(Credits {
            started: None,
            model: None,
        })
    );
}

/// THE NOTICES OF A MODEL BILLED PER REQUEST WITH NO DIALOG (READ in the
/// 2.1.301 to 2.1.304 bundles, the `fable-usage-credits` notification: a
/// credits-only model whose consent Claude Code stored, or on a tier an admin
/// bills, goes onto usage credits with no box, the footer's item never
/// drawn), and lower-priority mode's end as credits take over, each in place
/// of the measured lasting notice on its mode row: credits in use, the
/// model's named. Probe P1 of the review of 2026-10-19: these read as no
/// credits before, so nothing held. NEGATIVE CONTROLS: the measured notice
/// (the account's window, no model); the same words short of the edge, in
/// the model's reply, or under no model's name; the row with no notice.
#[test]
fn a_model_billed_to_credits_with_no_dialog_reads_as_credits_in_use() {
    let rows = screen(CREDITS_IN_USE_2_1_300);
    let mode = row_of(&rows, "auto mode on");
    let width = 110;
    // On the mode row's right where it fits, else on a row of its own.
    let on_mode = |notice: &str| {
        let lead = "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents";
        let room = width - 2 - lead.chars().count();
        let row = if notice.chars().count() + 2 <= room {
            format!("{lead}{notice:>room$}")
        } else {
            format!("{notice:>w$}", w = width - 2)
        };
        with_row(rows.clone(), mode, &row)
    };
    assert_eq!(
        in_use(&rows),
        Some(Credits {
            started: None,
            model: None,
        }),
        "the measured notice"
    );
    for (notice, model) in [
        ("Fable 5.1 is drawing from usage credits", Some("Fable 5.1")),
        (
            "Fable 5.1 is now using usage credits instead of your plan limits",
            Some("Fable 5.1"),
        ),
        (
            "Extra usage is now covering your requests · lower-priority mode ended",
            None,
        ),
    ] {
        assert_eq!(
            in_use(&on_mode(notice)),
            Some(Credits {
                started: None,
                model: model.map(str::to_string),
            }),
            "{notice}"
        );
    }
    let bare = with_row(
        rows.clone(),
        mode,
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
    );
    assert_eq!(in_use(&bare), None, "the row with no notice");
    let reply = row_of(&rows, "Hi! I am the local lab stub");
    for (at, row) in [
        (
            mode,
            "  Fable 5.1 is drawing from usage credits".to_string(),
        ),
        (
            reply,
            format!(
                "⏺ {:>w$}",
                "Fable 5.1 is drawing from usage credits",
                w = width - 4
            ),
        ),
        (
            mode,
            format!("{:>w$}", " is drawing from usage credits", w = width - 2),
        ),
    ] {
        let base = if at == mode {
            rows.clone()
        } else {
            bare.clone()
        };
        assert_eq!(in_use(&with_row(base, at, &row)), None, "{row}");
    }
}

/// The words are Claude Code's only where it draws them: NEGATIVE CONTROLS —
/// the same words in a tool's output above the composer, in the model's
/// reply, in a statusLine's left segment with something else on its right,
/// and short of the right edge, are no credits; nor is any screen with no
/// composer frame, nor the watchdog's screen.
#[test]
fn credits_words_anywhere_else_are_no_credits() {
    let rows = screen(CREDITS_IN_USE_2_1_300);
    let mode = row_of(&rows, "auto mode on");
    let reply = row_of(&rows, "Hi! I am the local lab stub");
    let bare = with_row(
        rows.clone(),
        mode,
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
    );
    assert_eq!(
        in_use(&bare),
        None,
        "the control: the footer without the notice"
    );
    // A tool's output and the model's reply, quoting the notice at the edge.
    let width = 110;
    let edge = |lead: &str| {
        format!(
            "{lead}{:>w$}",
            "Now using usage credits",
            w = width - 2 - lead.chars().count()
        )
    };
    for row in [edge("  ⎿  "), edge("⏺ "), edge("     ")] {
        assert_eq!(in_use(&with_row(bare.clone(), reply, &row)), None, "{row}");
    }
    // A statusLine of the person's own on the footer's first row: its words on
    // the left, something else right of them; and alone, short of the edge.
    let left = format!(
        "  Now using usage credits{:>w$}",
        "◐ medium · /effort",
        w = width - 2 - 25
    );
    assert_eq!(in_use(&with_row(bare.clone(), mode, &left)), None, "{left}");
    assert_eq!(
        in_use(&with_row(bare.clone(), mode, "  Now using usage credits")),
        None
    );
    // A message a person queued under the status row, quoting it at the
    // edge (the fullscreen renderer's notice row is the same place).
    let full = screen(CREDITS_IN_USE_FULLSCREEN_2_1_300);
    let notice = row_of(&full, "Now using usage credits");
    let queued = format!("❯ {:>106}", "Now using usage credits");
    assert_eq!(in_use(&with_row(full.clone(), notice, &queued)), None);
    assert_eq!(
        in_use(&with_row(full.clone(), notice, "")),
        None,
        "the control: the live zone without the notice"
    );
    // No status row over the live zone: nothing above the composer is read.
    let done = row_of(&full, "done 8:44 AM");
    assert_eq!(in_use(&with_row(full, done, "")), None);
    // No frame: the footer's notice is no composer's.
    let top = rows
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the top rule");
    assert_eq!(in_use(&rows[..top]), None);
    assert_eq!(
        in_use(&screen(WALL_WATCHDOG_SESSION_STATUSLINE_2_1_300)),
        None
    );
}

/// The vendor's warning, measured (`You've used 80% of your weekly limit ·
/// resets Oct 18 at 8:50pm (…)`) and as the lab's E2 drew the session one on
/// a statusLine's row (`… You've used 92% of your session limit · resets
/// 4:12am (…)`), is read with its window, figure and reset. NEGATIVE
/// CONTROLS: a model's or the credits' own figure, the words in a tool's
/// output, a figure past 100, and the credits screens, are no warning.
#[test]
fn the_vendors_usage_warning_is_read_with_its_window_and_figure() {
    let rows = screen(USAGE_WARNING_WEEKLY_2_1_300);
    assert_eq!(
        usage_warning(&rows),
        Some(Warning {
            window: Window::Weekly,
            pct: 80,
            reset: Some("Oct 18 at 8:50pm (America/Los_Angeles)".to_string()),
        })
    );
    let at = row_of(&rows, "You've used");
    let session = format!(
        "  TAP#3 5h=100 7d=51{:>w$}",
        "You've used 92% of your session limit · resets 4:12am (America/Los_Angeles)",
        w = 110 - 2 - 19
    );
    assert_eq!(
        usage_warning(&with_row(rows.clone(), at, &session)),
        Some(Warning {
            window: Window::Session,
            pct: 92,
            reset: Some("4:12am (America/Los_Angeles)".to_string()),
        })
    );
    for other in [
        "You've used 92% of your Opus limit · resets 4:12am",
        "You've used 40% of your usage credits",
        "You've used 120% of your weekly limit · resets 4:12am",
        "You've used most of your weekly limit",
    ] {
        let row = format!("{other:>108}");
        assert_eq!(
            usage_warning(&with_row(rows.clone(), at, &row)),
            None,
            "{other}"
        );
    }
    let reply = row_of(&rows, "Hi! I am the local lab stub");
    let quoted = format!(
        "  ⎿  {:>103}",
        "You've used 80% of your weekly limit · resets 4:12am"
    );
    let bare = with_row(rows.clone(), at, "");
    assert_eq!(
        usage_warning(&bare),
        None,
        "the control: the footer without it"
    );
    assert_eq!(usage_warning(&with_row(bare, reply, &quoted)), None);
    for credits in [CREDITS_STARTED_2_1_300, CREDITS_IN_USE_2_1_300] {
        assert_eq!(usage_warning(&screen(credits)), None);
    }
}

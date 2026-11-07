// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::prompt::fixtures::{
    CREDITS_IN_USE_2_1_300, CREDITS_IN_USE_80_2_1_300, CREDITS_IN_USE_FULLSCREEN_2_1_300,
    CREDITS_STARTED_2_1_300, CREDITS_STARTED_80_2_1_300, CREDITS_STARTED_FULLSCREEN_2_1_300,
    FABLE_WARNING_2_1_301, IDLE_AFTER_LIMIT_AND_MODEL_SWITCH, USAGE_WARNING_WEEKLY_2_1_300,
    WALL_FABLE_NO_RESET_2_1_301, WALL_FABLE_RESET_2_1_301,
    WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300, WALL_WATCHDOG_SESSION_STATUSLINE_2_1_300,
    screen,
};

fn with_row(mut rows: Vec<String>, i: usize, row: &str) -> Vec<String> {
    rows[i] = row.to_string();
    rows
}

fn row_of(rows: &[String], text: &str) -> usize {
    rows.iter()
        .position(|r| r.contains(text))
        .unwrap_or_else(|| panic!("no row holds {text:?}: {rows:#?}"))
}

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
    // 2.1.312's words for the same notice (`Your ${Wce[type]} resets`, `5-hour limit` for
    // `session limit`), one column further in so the notice still ends at the frame's edge.
    let five_hour = CREDITS_STARTED_2_1_300.replace(
        "You're now using usage credits · Your session limit resets",
        " You're now using usage credits · Your 5-hour limit resets",
    );
    assert_ne!(five_hour, CREDITS_STARTED_2_1_300, "the notice row");
    assert_eq!(
        in_use(&screen(&five_hour)),
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

#[test]
fn a_model_billed_to_credits_with_no_dialog_reads_as_credits_in_use() {
    let rows = screen(CREDITS_IN_USE_2_1_300);
    let mode = row_of(&rows, "auto mode on");
    let width = 110;
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
    let full = screen(CREDITS_IN_USE_FULLSCREEN_2_1_300);
    let notice = row_of(&full, "Now using usage credits");
    let queued = format!("❯ {:>106}", "Now using usage credits");
    assert_eq!(in_use(&with_row(full.clone(), notice, &queued)), None);
    assert_eq!(
        in_use(&with_row(full.clone(), notice, "")),
        None,
        "the control: the live zone without the notice"
    );
    let done = row_of(&full, "done 8:44 AM");
    assert_eq!(in_use(&with_row(full, done, "")), None);
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
    // 2.1.312 names the five-hour window `5-hour limit` (`Wce.five_hour` @225060028 in the
    // store's 2.1.312 binary), the builds before it `session limit`: either is the session's.
    for name in ["session limit", "5-hour limit"] {
        let session = format!(
            "  TAP#3 5h=100 7d=51{:>w$}",
            format!("You've used 92% of your {name} · resets 4:12am (America/Los_Angeles)"),
            w = 110 - 2 - 19
        );
        assert_eq!(
            usage_warning(&with_row(rows.clone(), at, &session)),
            Some(Warning {
                window: Window::Session,
                pct: 92,
                reset: Some("4:12am (America/Los_Angeles)".to_string()),
            }),
            "{name}"
        );
    }
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

fn week(model: WeekModel, pct: u8, reset: Option<&str>, wall: bool) -> Option<ModelWeek> {
    Some(ModelWeek {
        model,
        pct,
        reset: reset.map(str::to_string),
        wall,
    })
}

#[test]
fn the_measured_fable_and_sonnet_weeks_read_with_their_figure_and_reset() {
    assert_eq!(
        model_week(&screen(FABLE_WARNING_2_1_301)),
        week(
            WeekModel::Fable,
            85,
            Some("Oct 19 at 11:36pm (America/Los_Angeles)"),
            false
        )
    );
    assert_eq!(
        usage_warning(&screen(FABLE_WARNING_2_1_301)),
        None,
        "the Fable week is no session or weekly window"
    );
    assert_eq!(
        model_week(&screen(WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300)).map(|w| (
            w.model,
            w.pct,
            w.wall,
            w.reset.is_some()
        )),
        Some((WeekModel::Sonnet, 100, true, true))
    );
    let sonnet = model_week(&screen(WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300))
        .and_then(|w| w.reset)
        .expect("the Sonnet wall's reset");
    assert!(sonnet.starts_with("Oct 17 at 4:20pm"), "{sonnet}");
}

#[test]
fn a_fable_wall_reads_full_over_the_footers_older_warning() {
    let rows = screen(WALL_FABLE_RESET_2_1_301);
    assert_eq!(
        model_week(&rows),
        week(
            WeekModel::Fable,
            100,
            Some("1:36am (America/Los_Angeles)"),
            true
        )
    );
    let at = row_of(&rows, "You've hit your Fable limit");
    let unwalled: Vec<String> = rows
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != at)
        .map(|(_, r)| r.clone())
        .collect();
    assert_eq!(
        model_week(&unwalled),
        week(
            WeekModel::Fable,
            85,
            Some("Oct 19 at 11:36pm (America/Los_Angeles)"),
            false
        ),
        "the control: without the wall's row the footer's warning reads"
    );
}

/// Both measured walls that name no reset: 2.1.301's `You've hit your Fable limit`, and the
/// usage-credits form (`You've reached your Fable limit. Run /usage-credits …`) as the screen was
/// when its turn ended on it, the capture's later `/model` and its answer taken out: a wall is
/// read where the turn ended on it ([`crate::wall::wall`]).
#[test]
fn a_fable_wall_that_names_no_reset_reads_full_with_none() {
    let measured = screen(IDLE_AFTER_LIMIT_AND_MODEL_SWITCH);
    let model = row_of(&measured, "❯ /model");
    let at_turn_end: Vec<String> = measured[..model]
        .iter()
        .chain(&measured[model + 2..])
        .cloned()
        .collect();
    for (name, rows) in [
        ("the 2.1.301 wall", screen(WALL_FABLE_NO_RESET_2_1_301)),
        ("the usage-credits wall", at_turn_end),
    ] {
        assert_eq!(
            model_week(&rows),
            week(WeekModel::Fable, 100, None, true),
            "{name}"
        );
    }
}

#[test]
fn no_unmeasured_name_and_no_programs_words_read_as_a_models_week() {
    let rows = screen(FABLE_WARNING_2_1_301);
    let at = row_of(&rows, "You've used 85%");
    for name in ["Opus limit", "Sonnet limit", "usage credits"] {
        let other = rows[at].replace("Fable limit", name);
        assert_ne!(other, rows[at], "the warning row");
        assert_eq!(
            model_week(&with_row(rows.clone(), at, &other)),
            None,
            "{other}"
        );
    }
    let numbered = rows[at].replace("your Fable limit", "your Fable 5 limit");
    assert_eq!(
        model_week(&with_row(rows.clone(), at, &numbered)),
        None,
        "{numbered}"
    );
    assert_eq!(model_week(&screen(USAGE_WARNING_WEEKLY_2_1_300)), None);
    assert_eq!(model_week(&screen(CREDITS_IN_USE_2_1_300)), None);
    let walled = screen(WALL_FABLE_RESET_2_1_301);
    let wall = row_of(&walled, "You've hit your Fable limit");
    let footer = row_of(&walled, "You've used 85%");
    let quiet = with_row(walled.clone(), footer, "");
    assert!(
        model_week(&quiet).is_some(),
        "the control: the wall reads with no warning"
    );
    let prompt = wall - 1;
    for (said, quoted) in [
        (
            "⏺ Bash(cat notes.txt)",
            "  ⎿  You've hit your Fable limit · resets 1:36am (America/Los_Angeles)",
        ),
        (
            "⏺ The notes quote Claude Code's wall:",
            "  You've hit your Fable limit · resets 1:36am (America/Los_Angeles)",
        ),
    ] {
        let rows = with_row(with_row(quiet.clone(), prompt, said), wall, quoted);
        assert_eq!(model_week(&rows), None, "{said} {quoted}");
    }
    for other in [
        "  ⎿  You've hit your Opus limit · resets 1:36am (America/Los_Angeles)",
        "  ⎿  You've hit your Fable 5 limit · resets 1:36am (America/Los_Angeles)",
    ] {
        assert_eq!(
            model_week(&with_row(quiet.clone(), wall, other)),
            None,
            "{other}"
        );
    }
}

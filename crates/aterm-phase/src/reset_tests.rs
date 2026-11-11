// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::prompt::fixtures::{
    LIMIT_OPTIONS_DIALOG, LIMIT_OPTIONS_PANEL, limit_options_dialog, limit_options_panel, screen,
};
use crate::prompt::{Role, Select, parse_prompt_v2};

const STOP: &str = "Stop and wait for limit to reset";
const WAIT: &str = "Wait here, then continue automatically at Oct 9 at 5am";
const CREDITS: &str = "Switch to usage credits";
const GRANT: &str = "Use a weekly reset (2 left, until Oct 5)";
const SESSION: &str = "Use a 5-hour reset (2 left, until Oct 5)";
const FULL: &str = "Use a full reset (1 left, until Oct 5)";
const FABLE: &str = "Use a Fable reset (1 left, until Oct 5)";
const FABLE_AT_WEEK: &str =
    "Use a Fable reset (1 left, until Oct 5) · frees up only what Fable used this week";
const WEEKLY: &str = "Reset your session limit now";
const WEEKLY_2_1_312: &str = "Reset your 5-hour limit now";
const BODY: &str = "Refills your weekly limit now · your weekly reset day stays Oct 9 at 5am";
const DETAIL: &str = "2 weekly resets left · use by Oct 5 at 3pm";
const ASKS: &str = " · send a message to continue";
const OLD_GRANT: &str = "Use your reset now (2 left, until Oct 5)";
const OLD_DETAIL: &str = "2 resets left · use by Oct 5 at 3pm";

fn boxed(rows: &[String]) -> PromptV2 {
    parse_prompt_v2(rows).unwrap_or_else(|| panic!("a box: {rows:#?}"))
}

fn row_among(labels: &[&str]) -> Option<ResetRow> {
    reset_row(&boxed(&limit_options_panel(labels, 0, false)), &[])
}

#[test]
fn the_measured_dialog_with_no_reset_on_offer_has_no_grant_row() {
    use crate::prompt::fixtures::{
        LIMIT_DIALOG_NO_RESET_ROWS_2_1_301, LIMIT_DIALOG_NO_RESET_ROWS_2_1_304,
    };
    for (fixture, options) in [
        (LIMIT_DIALOG_NO_RESET_ROWS_2_1_301, 4),
        (LIMIT_DIALOG_NO_RESET_ROWS_2_1_304, 3),
    ] {
        let dialog = boxed(&screen(fixture));
        assert_eq!(reset_row(&dialog, &[]), None);
        assert_eq!(dialog.options.len(), options, "{:?}", dialog.options);
        assert!(dialog.options.iter().all(|o| !spends_reset(&o.label)));
    }
    let measured = screen(LIMIT_DIALOG_NO_RESET_ROWS_2_1_304);
    let stop = measured
        .iter()
        .position(|row| row.trim_start().starts_with("❯ 1. Stop and wait"))
        .expect("the stop row");
    let offered = |rows: &[&str]| -> Vec<String> {
        let mut out = measured[..=stop].to_vec();
        out.extend(
            rows.iter()
                .enumerate()
                .map(|(k, label)| format!("    {}. {label}", k + 2)),
        );
        let n = rows.len();
        out.extend(measured[stop + 1..].iter().map(|row| {
            row.replacen("    2. Wait", &format!("    {}. Wait", 2 + n), 1)
                .replacen("    3. Upgrade", &format!("    {}. Upgrade", 3 + n), 1)
        }));
        out
    };
    for (rows, index, label, kind, left, day) in [
        (
            &["Use a full reset (2 left, until Oct 5)"][..],
            1,
            "Use a full reset (2 left, until Oct 5)",
            ResetKind::Full,
            2,
            "Oct 5",
        ),
        (&[SESSION], 1, SESSION, ResetKind::Session, 2, "Oct 5"),
        (
            &["Use a weekly reset (1 left, until Jan 2, 2027)"],
            1,
            "Use a weekly reset (1 left, until Jan 2, 2027)",
            ResetKind::Weekly,
            1,
            "Jan 2, 2027",
        ),
    ] {
        let dialog = boxed(&offered(rows));
        assert_eq!(dialog.options.len(), 3 + rows.len(), "{:?}", dialog.options);
        let row = reset_row(&dialog, &[]).unwrap_or_else(|| panic!("the offered row: {rows:?}"));
        assert_eq!(
            (
                row.index,
                row.label.as_str(),
                row.kind,
                row.left,
                row.day.as_str()
            ),
            (index, label, kind, left, day),
            "{rows:?}"
        );
    }
    for rows in [
        &[SESSION, "Use a full reset (2 left, until Oct 5)"][..],
        &[
            FABLE_AT_WEEK,
            "Use a weekly reset (1 left, until Jan 2, 2027)",
        ],
        &[FABLE_AT_WEEK],
    ] {
        let dialog = boxed(&offered(rows));
        assert_eq!(dialog.options.len(), 3 + rows.len(), "{:?}", dialog.options);
        assert_eq!(reset_row(&dialog, &[]), None, "{rows:?}");
    }
}

fn confirm_with(body: &[&str]) -> Vec<String> {
    let base = screen(fixtures::CONFIRM);
    assert_eq!(
        [base[5].trim(), base[6].trim()],
        [BODY, DETAIL],
        "the fixture's rows"
    );
    let mut out = base[..5].to_vec();
    out.extend(body.iter().map(|row| format!("   {row}")));
    out.extend(base[7..].iter().cloned());
    out
}

fn confirm_of(title: &str, body: &[&str], yes: &str, no: &str) -> Vec<String> {
    let base = screen(fixtures::CONFIRM);
    let mut out = base[..3].to_vec();
    out.push(format!("   {title}"));
    out.push(String::new());
    out.extend(body.iter().map(|row| format!("   {row}")));
    out.push(String::new());
    out.push(format!("   ❯ {yes}"));
    out.push(format!("     {no}"));
    out.extend(base[10..].iter().cloned());
    out
}

fn confirm_read(rows: &[String]) -> ConfirmRead {
    reset_confirm(&boxed(rows), rows)
}

fn inline(rows: &[String]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            if is_panel_rule(row) {
                "─".repeat(row.chars().count())
            } else {
                row.strip_prefix(' ').unwrap_or(row).to_string()
            }
        })
        .collect()
}

#[test]
fn every_kind_is_drawn_by_its_own_words() {
    for (kind, word, name, capital, done, title, yes) in [
        (
            ResetKind::Session,
            "session",
            "5-hour reset",
            "5-hour reset",
            "5-hour limit reset",
            "Use a 5-hour reset?",
            "Yes, use my 5-hour reset",
        ),
        (
            ResetKind::Weekly,
            "weekly",
            "weekly reset",
            "Weekly reset",
            "Weekly limits reset",
            "Use a weekly reset?",
            "Yes, use my weekly reset",
        ),
        (
            ResetKind::Fable,
            "fable",
            "Fable reset",
            "Fable reset",
            "Fable reset used · what Fable used is back in your weekly limit",
            "Use a Fable reset?",
            "Yes, use my Fable reset",
        ),
        (
            ResetKind::Full,
            "full",
            "full reset",
            "Full reset",
            "Limits reset",
            "Use a full reset?",
            "Yes, use my full reset",
        ),
    ] {
        assert_eq!(
            (
                kind.word(),
                kind.name(),
                kind.capital().as_str(),
                kind.done()
            ),
            (word, name, capital, done)
        );
        assert_eq!((kind.title(), kind.yes()), (title.into(), yes.into()));
        assert_eq!(ResetKind::of_word(word), Some(kind));
        assert_eq!(reset_title(title), Some(kind));
        assert_eq!(reset_title(&format!("   {title}  ")), Some(kind));
        assert_eq!(kind.takeable(), kind != ResetKind::Fable);
    }
    for (kind, none, one, many) in [
        (
            ResetKind::Session,
            "no five-hour resets",
            "1 five-hour reset",
            "12 five-hour resets",
        ),
        (
            ResetKind::Weekly,
            "no weekly resets",
            "1 weekly reset",
            "12 weekly resets",
        ),
        (
            ResetKind::Fable,
            "no Fable resets",
            "1 Fable reset",
            "12 Fable resets",
        ),
        (
            ResetKind::Full,
            "no full resets",
            "1 full reset",
            "12 full resets",
        ),
    ] {
        assert_eq!(
            [none, one, many].map(|t| kind.resets_of(t)),
            [Some(0), Some(1), Some(12)],
            "{kind:?}"
        );
        assert_eq!(
            [0, 1, 12].map(|n| kind.resets(n)),
            [none, one, many].map(str::to_owned),
            "{kind:?}: the record's words are the vendor's"
        );
    }
    for wrong in [
        "1 resets",
        "2 resets",
        "no resets",
        "1 reset",
        "0 weekly resets",
        "1 weekly resets",
        "2 weekly reset",
        "02 weekly resets",
        "two weekly resets",
        "2 weekly resets left",
    ] {
        assert_eq!(ResetKind::Weekly.resets_of(wrong), None, "{wrong}");
    }
    assert_eq!(ResetKind::of_word("5-hour"), None);
    assert_eq!(ResetKind::Session.resets_of("2 5-hour resets"), None);
    assert_eq!(ResetKind::Weekly.resets_of("2 five-hour resets"), None);
    for title in [
        "Use your reset?",
        "Use a reset?",
        "Use a weekly reset",
        "Use a hourly reset?",
    ] {
        assert_eq!(reset_title(title), None, "{title}");
    }
}

#[test]
fn the_grant_row_is_read_by_its_exact_label_never_by_its_place() {
    let rows = screen(fixtures::DIALOG_GRANT);
    assert_eq!(
        rows,
        limit_options_panel(&[STOP, GRANT, WAIT, CREDITS], 0, false),
        "the fixture is the wall dialog's frame"
    );
    let p = boxed(&rows);
    assert_eq!((p.kind, p.select), (PromptKind::UsageLimit, Select::Digits));
    assert_eq!(p.options[1].role, Role::Other);
    assert_eq!(
        reset_row(&p, &[]),
        Some(ResetRow {
            index: 1,
            label: GRANT.to_string(),
            kind: ResetKind::Weekly,
            left: 2,
            day: "Oct 5".to_string(),
        })
    );
    for (labels, index) in [
        (&[GRANT, STOP, WAIT, CREDITS][..], 0),
        (&[STOP, WAIT, GRANT, CREDITS], 2),
        (&[CREDITS, STOP, WAIT, GRANT], 3),
        (&[STOP, GRANT], 1),
    ] {
        assert_eq!(
            row_among(labels).map(|r| r.index),
            Some(index),
            "{labels:?}"
        );
    }
    let focused = boxed(&limit_options_panel(&[STOP, GRANT, WAIT], 1, true));
    assert_eq!(reset_row(&focused, &[]).map(|r| r.index), Some(1));
    let inline_frame = boxed(&limit_options_dialog(&[STOP, GRANT, WAIT, CREDITS], 0));
    assert_eq!(reset_row(&inline_frame, &[]).map(|r| r.left), Some(2));
    for (label, kind, left, day) in [
        (SESSION, ResetKind::Session, 2, "Oct 5"),
        (FULL, ResetKind::Full, 1, "Oct 5"),
        (
            "Use a weekly reset (1 left, until Dec 31)",
            ResetKind::Weekly,
            1,
            "Dec 31",
        ),
        (
            "Use a 5-hour reset (12 left, until Jan 2, 2027)",
            ResetKind::Session,
            12,
            "Jan 2, 2027",
        ),
    ] {
        let read = row_among(&[STOP, label, WAIT]).expect(label);
        assert_eq!(
            (read.kind, read.left, read.day.as_str(), read.label.as_str()),
            (kind, left, day, label)
        );
    }

    assert_eq!(
        reset_row(&boxed(&screen(fixtures::DIALOG_WEEKLY)), &[]),
        None
    );
    assert_eq!(reset_row(&boxed(&screen(fixtures::DIALOG_BARE)), &[]), None);
    assert_eq!(
        row_among(&[STOP, WEEKLY, GRANT, WAIT]),
        None,
        "both rows in one menu"
    );
    assert_eq!(
        row_among(&[STOP, GRANT, WAIT, WEEKLY]),
        None,
        "the weekly session reset's row anywhere"
    );
    assert_eq!(row_among(&[STOP, WAIT, CREDITS]), None, "no grant row");
    assert_eq!(
        row_among(&[STOP, GRANT, GRANT, WAIT]),
        None,
        "one kind drawn twice"
    );
    assert_eq!(
        row_among(&[
            STOP,
            SESSION,
            GRANT,
            "Use a 5-hour reset (1 left, until Oct 9)",
            WAIT
        ]),
        None,
        "one kind drawn twice, the second unread"
    );
    assert_eq!(
        row_among(&[STOP, FABLE, GRANT, FABLE_AT_WEEK, WAIT]),
        None,
        "the Fable kind drawn twice"
    );
    for label in [
        "Use a weekly reset (0 left, until Oct 5)",
        "Use a weekly reset (2 left)",
        "Use a weekly reset (until Oct 5)",
        "Use a weekly reset (two left, until Oct 5)",
        "Use a weekly reset (02 left, until Oct 5)",
        "Use a weekly reset (-2 left, until Oct 5)",
        "Use a weekly reset (2 left, until tomorrow)",
        "Use a weekly reset (2 left, until Oct 5 at 3pm)",
        "Use a weekly reset (2 left, until Oct 32)",
        "Use a weekly reset (2 left, until Oct 5, 27)",
        "Use a weekly reset (2 left, until Oct 5 (UTC))",
        "Use a weekly reset (2 left, until Oct 5) and continue",
        "Use a weekly reset (2 left, until Oct 5) · frees up only what Fable used this week",
        "Use a weekly reset — 2 left, until Oct 5",
        "use a weekly reset (2 left, until Oct 5)",
        "Use a weekly resets (2 left, until Oct 5)",
        "Use the weekly reset (2 left, until Oct 5)",
        "Use a reset now (2 left, until Oct 5)",
        OLD_GRANT,
        "Use your reset now",
    ] {
        assert_eq!(row_among(&[STOP, label, WAIT, CREDITS]), None, "{label}");
    }
    let mut retitled = rows.clone();
    retitled[3] = "   What would you like?".to_string();
    assert_ne!(boxed(&retitled).kind, PromptKind::UsageLimit);
    assert_eq!(reset_row(&boxed(&retitled), &[]), None);
    for edit in [
        (|p: &mut PromptV2| p.options[0].n = None) as fn(&mut PromptV2),
        |p| p.options[2].n = Some(4),
        |p| p.options[1].focused = true,
        |p| p.head_off_screen = true,
        |p| p.kind = PromptKind::Other,
    ] {
        let mut edited = p.clone();
        edit(&mut edited);
        assert_eq!(reset_row(&edited, &[]), None, "{edited:?}");
    }
}

#[test]
fn a_row_is_taken_only_as_the_one_reset_on_offer_and_never_a_fable_row() {
    const UNKNOWN: &str = "Use a scheduled full weekly reset (1 left, until Oct 7)";
    assert!(uses_a_reset(UNKNOWN), "an unknown kind still spends");
    let kinds = boxed(&screen(fixtures::DIALOG_KINDS));
    assert_eq!(kinds.options.len(), 6, "{:?}", kinds.options);
    assert_eq!(reset_row(&kinds, &[]), None, "three kinds on offer");
    let fable = boxed(&screen(fixtures::DIALOG_FABLE));
    assert_eq!(fable.options[1].label, FABLE);
    assert_eq!(
        reset_row(&fable, &[]),
        None,
        "a weekly reset beside a Fable one"
    );
    for labels in [
        &[STOP, GRANT, SESSION, WAIT][..],
        &[STOP, FULL, GRANT, WAIT],
        &[STOP, SESSION, FULL, WAIT],
        &[STOP, FABLE, FULL, WAIT],
        &[FABLE, STOP, WAIT, SESSION],
        &[STOP, GRANT, FABLE_AT_WEEK, WAIT],
        &[STOP, FABLE_AT_WEEK, GRANT, WAIT],
        &[STOP, "Use a Fable reset", GRANT, WAIT],
        &[STOP, FABLE, "Use a weekly reset (1 left)", FULL, WAIT],
        &[STOP, "Use a 5-hour reset", GRANT, WAIT],
        &[STOP, UNKNOWN, FULL, WAIT],
        &[STOP, FULL, UNKNOWN, WAIT],
        &[STOP, GRANT, OLD_GRANT, WAIT],
        &[
            STOP,
            SESSION,
            "Use a full reset plus (1 left, until Oct 5)",
            WAIT,
        ],
        &[
            STOP,
            "use a 5-hour resetting (2 left, until Oct 5)",
            "Use a weekly reset (1 left, until Oct 9)",
            WAIT,
        ],
    ] {
        assert_eq!(row_among(labels), None, "several on offer: {labels:?}");
        for (index, label) in labels.iter().enumerate() {
            let alone: Vec<&str> = labels
                .iter()
                .enumerate()
                .filter(|(k, l)| *k == index || !spends_reset(l) && !uses_a_reset(l))
                .map(|(_, l)| *l)
                .collect();
            if let Some(kind) = grant_kind(label).filter(|k| k.takeable())
                && grant_of(label, kind).is_some()
            {
                assert_eq!(
                    row_among(&alone).map(|r| (r.kind, r.label)),
                    Some((kind, (*label).to_string())),
                    "{alone:?}"
                );
            }
        }
    }
    for label in [
        FABLE,
        FABLE_AT_WEEK,
        UNKNOWN,
        OLD_GRANT,
        "Use a full reset plus (1 left, until Oct 5)",
        "use a 5-hour resetting (2 left, until Oct 5)",
        "use a 5-hour reset (2 left, until Oct 5)",
        "Use a 5-hour and weekly reset (1 left, until Oct 7)",
    ] {
        assert_eq!(row_among(&[STOP, label, WAIT]), None, "{label}");
    }
}

#[test]
fn under_the_owners_order_the_first_listed_kind_on_offer_is_taken() {
    use ResetKind::{Fable, Full, Session, Weekly};
    let kinds = boxed(&screen(fixtures::DIALOG_KINDS));
    let fable = boxed(&screen(fixtures::DIALOG_FABLE));
    let taken = |p: &PromptV2, order: &[ResetKind]| {
        reset_row(p, order).map(|row| (row.index, row.kind, row.left, row.day))
    };
    assert_eq!(taken(&kinds, &[]), None, "the lone-offer rule");
    assert_eq!(taken(&fable, &[]), None, "the lone-offer rule");
    let at = |p: &PromptV2, label: &str| {
        p.options
            .iter()
            .position(|o| o.label == label)
            .unwrap_or_else(|| panic!("{label}: {:?}", p.options))
    };
    for (order, label, kind, left, day) in [
        (&[Full, Weekly, Session][..], FULL, Full, 1, "Oct 5"),
        (
            &[Weekly, Full],
            "Use a weekly reset (1 left, until Oct 9)",
            Weekly,
            1,
            "Oct 9",
        ),
        (&[Session], SESSION, Session, 2, "Oct 5"),
        (&[Fable, Session, Full], SESSION, Session, 2, "Oct 5"),
    ] {
        assert_eq!(
            taken(&kinds, order),
            Some((at(&kinds, label), kind, left, day.to_string())),
            "{order:?}"
        );
    }
    let weekly = "Use a weekly reset (1 left, until Oct 9)";
    assert_eq!(
        taken(&fable, &[Weekly]),
        Some((at(&fable, weekly), Weekly, 1, "Oct 9".to_string())),
        "a weekly reset beside a Fable one"
    );
    for order in [&[Full][..], &[Session, Full], &[Fable]] {
        assert_eq!(
            taken(&fable, order),
            None,
            "nothing listed on offer: {order:?}"
        );
    }
    let among = |labels: &[&str], order: &[ResetKind]| {
        reset_row(&boxed(&limit_options_panel(labels, 0, false)), order)
            .map(|row| (row.kind, row.label))
    };
    for first in ["Use a full reset", "Use a full reset (0 left, until Oct 5)"] {
        assert_eq!(
            among(&[STOP, first, GRANT, WAIT], &[Full, Weekly]),
            None,
            "{first}"
        );
        assert_eq!(
            among(&[STOP, first, GRANT, WAIT], &[Weekly, Full]),
            Some((Weekly, GRANT.to_string())),
            "{first}: the first listed kind is the weekly one"
        );
    }
    for labels in [
        &[STOP, GRANT, GRANT, WAIT][..],
        &[
            STOP,
            GRANT,
            "Use a weekly reset (1 left, until Oct 9)",
            WAIT,
        ],
        &[
            STOP,
            FULL,
            "Use a scheduled full weekly reset (1 left, until Oct 7)",
            WAIT,
        ],
        &[STOP, FULL, OLD_GRANT, WAIT],
        &[STOP, FULL, WEEKLY, WAIT],
    ] {
        for order in [&[Full, Weekly, Session][..], &[Weekly], &[Full]] {
            assert_eq!(among(labels, order), None, "{labels:?} under {order:?}");
        }
    }
    assert_eq!(among(&[STOP, FABLE, WAIT], &[Fable, Full]), None);
    assert_eq!(
        among(&[STOP, FABLE, FULL, WAIT], &[Fable, Full]),
        Some((Full, FULL.to_string()))
    );
}

#[test]
fn a_row_opens_as_a_kind_only_where_the_kinds_head_is_a_word_of_its_own() {
    for (label, kind) in [
        (SESSION, ResetKind::Session),
        (GRANT, ResetKind::Weekly),
        (FULL, ResetKind::Full),
        (FABLE, ResetKind::Fable),
        (FABLE_AT_WEEK, ResetKind::Fable),
        ("Use a weekly reset", ResetKind::Weekly),
        (
            "Use a Fable reset · frees up only what Fable used this week",
            ResetKind::Fable,
        ),
        (
            "use a 5-hour reset (2 left, until Oct 5)",
            ResetKind::Session,
        ),
        ("USE A FULL RESET (1 left, until Oct 5)", ResetKind::Full),
    ] {
        assert_eq!(grant_kind(label), Some(kind), "{label}");
    }
    for label in [
        "Use a full reset plus (1 left, until Oct 5)",
        "Use a full resets (1 left, until Oct 5)",
        "use a 5-hour resetting (2 left, until Oct 5)",
        "Use a weekly reset— 1 left",
        "Use a weekly",
        OLD_GRANT,
        WEEKLY,
        STOP,
        WAIT,
        "",
    ] {
        assert_eq!(grant_kind(label), None, "{label}");
    }
}

/// 2.1.312's once-a-week row (`Reset your 5-hour limit now`) spends, as 2.1.311's does: it is
/// never the row taken, and a menu that holds either, alone or beside a grant row, gives no
/// row under any order.
#[test]
fn claude_2_1_312s_once_a_week_row_spends_and_is_never_taken() {
    use ResetKind::{Full, Session, Weekly};
    for row in [WEEKLY, WEEKLY_2_1_312] {
        assert!(spends_reset(row), "{row}");
        assert_eq!(grant_kind(row), None, "{row}");
    }
    assert_eq!(WEEKLY_ROW_BEFORE_2_1_312, WEEKLY);
    assert_eq!(crate::anchor!("reset.weekly_row"), WEEKLY_2_1_312);
    let orders: [&[ResetKind]; 4] = [&[], &[Session], &[Full, Weekly, Session], &[Weekly]];
    for (name, fixture) in [
        ("alone", fixtures::DIALOG_WEEKLY_2_1_312),
        (
            "beside a grant row",
            fixtures::DIALOG_WEEKLY_BESIDE_GRANT_2_1_312,
        ),
    ] {
        let dialog = boxed(&screen(fixture));
        assert_eq!(dialog.kind, PromptKind::UsageLimit, "{name}");
        assert!(
            dialog.options.iter().any(|o| o.label == WEEKLY_2_1_312),
            "{name}: {:?}",
            dialog.options
        );
        for order in orders {
            assert_eq!(reset_row(&dialog, order), None, "{name} under {order:?}");
        }
    }
    let beside = boxed(&screen(fixtures::DIALOG_WEEKLY_BESIDE_GRANT_2_1_312));
    assert_eq!(beside.options[1].label, SESSION);
    let kept: Vec<&str> = beside
        .options
        .iter()
        .map(|o| o.label.as_str())
        .filter(|label| *label != WEEKLY_2_1_312)
        .collect();
    assert_eq!(
        row_among(&kept).map(|r| (r.kind, r.label)),
        Some((Session, SESSION.to_string())),
        "the same menu with the once-a-week row cut: {kept:?}"
    );
    for weekly in [WEEKLY, WEEKLY_2_1_312] {
        for labels in [
            &[STOP, weekly, SESSION, WAIT][..],
            &[STOP, SESSION, WAIT, weekly],
            &[STOP, FULL, weekly, GRANT, WAIT],
            &[STOP, weekly, WAIT],
        ] {
            let menu = boxed(&limit_options_panel(labels, 0, false));
            for order in orders {
                assert_eq!(reset_row(&menu, order), None, "{labels:?} under {order:?}");
            }
        }
    }
    for (name, text) in fixtures::ALL {
        let Some(p) = parse_prompt_v2(&screen(text)) else {
            continue;
        };
        for order in orders {
            if let Some(row) = reset_row(&p, order) {
                assert!(
                    grant_kind(&row.label).is_some()
                        && !opens(&row.label, WEEKLY)
                        && !opens(&row.label, WEEKLY_2_1_312),
                    "{name}: {row:?}"
                );
            }
        }
    }
}

#[test]
fn a_label_that_spends_a_reset_is_told_by_its_opening_words() {
    for label in [
        GRANT,
        SESSION,
        FULL,
        FABLE,
        FABLE_AT_WEEK,
        "Use a weekly reset",
        "Use a 5-hour reset (…)",
        "use a full reset — 1 left",
        "  Use a weekly reset (2 left, until Oct 5)",
        WEEKLY,
        "Reset your session limit now (1/week)",
        WEEKLY_2_1_312,
        "Reset your 5-hour limit now (1/week)",
        "  reset your 5-hour limit now",
        "Yes, use my 5-hour reset",
        "Yes, use my weekly reset",
        "Yes, use my Fable reset",
        "YES, USE MY FULL RESET",
        "Yes, use my weekly reset now",
    ] {
        assert!(spends_reset(label), "{label}");
    }
    for label in [
        STOP,
        WAIT,
        CREDITS,
        "No, keep it",
        "Yes",
        "Yes, proceed",
        "Yes, use my browser",
        "Yes, use my enabled plugins in cloud sessions",
        "Yes, use my",
        "Use a",
        "Use a different model",
        "Reset used · no resets left",
        "Reset the view",
        "Use your",
        "Use a fresh session and reset nothing later",
        "Use a new branch, then reset it",
        "Use my settings to reset the view",
        "Don’t continue automatically",
        "",
        OLD_GRANT,
        "Yes, use my reset",
    ] {
        assert!(!spends_reset(label), "{label}");
    }
    for label in [
        GRANT,
        SESSION,
        FULL,
        "Yes, use my weekly reset now",
        "Use a scheduled full weekly reset (1 left, until Oct 7)",
        "Use an unlisted reset, 1 left",
        "Use a 5-hour and weekly reset (1 left, until Oct 7)",
        "Use a reset credit (1 left)",
        OLD_GRANT,
        "Yes, use my reset",
    ] {
        assert!(uses_a_reset(label), "{label}");
    }
    for label in [
        "Use a fresh session and reset nothing later",
        "Use a fresh session (reset nothing later)",
        "Use a fresh session but reset nothing",
        "Use a new branch, then reset it",
        "Use my settings, then reset them",
        "Use my settings to reset the view",
        "Use a new session to reset the view",
        "Use a different model",
    ] {
        assert!(!uses_a_reset(label), "{label}");
    }
}

#[test]
fn the_confirmation_reads_exactly_plain_wrapped_and_with_the_focus_on_no() {
    let rows = screen(fixtures::CONFIRM);
    assert_eq!(rows, confirm_with(&[BODY, DETAIL]));
    let p = boxed(&rows);
    assert_eq!((p.kind, p.select), (PromptKind::Other, Select::ArrowsEnter));
    assert_eq!(
        p.options.iter().map(|o| o.role).collect::<Vec<_>>(),
        [Role::Other, Role::Deny]
    );
    let exact = ResetConfirm {
        title_row: 3,
        yes_row: 8,
        yes_focused: true,
        kind: ResetKind::Weekly,
        limits: vec!["weekly".to_string()],
        week: "Oct 9 at 5am".to_string(),
        left: 2,
        deadline: "Oct 5 at 3pm".to_string(),
        head: format!("{BODY} {DETAIL}"),
    };
    assert_eq!(reset_confirm(&p, &rows), ConfirmRead::Exact(exact.clone()));
    assert_eq!(rows[exact.title_row].trim(), "Use a weekly reset?");
    assert_eq!(rows[exact.yes_row].trim(), "❯ Yes, use my weekly reset");

    let narrow = screen(fixtures::CONFIRM_60);
    assert!(narrow.iter().all(|row| row.chars().count() <= 60));
    assert_eq!(narrow[6].trim(), "stays Oct 9 at 5am", "the wrapped row");
    assert_eq!(
        confirm_read(&narrow),
        ConfirmRead::Exact(ResetConfirm {
            yes_row: 9,
            ..exact.clone()
        })
    );
    assert_eq!(
        confirm_read(&inline(&rows)),
        ConfirmRead::Exact(exact.clone())
    );
    assert_eq!(
        confirm_read(&screen(fixtures::CONFIRM_FOCUS_NO)),
        ConfirmRead::Exact(ResetConfirm {
            yes_focused: false,
            ..exact.clone()
        })
    );
    for (fixture, kind, limits, left) in [
        (
            fixtures::CONFIRM_SESSION,
            ResetKind::Session,
            &["session"][..],
            2,
        ),
        (
            fixtures::CONFIRM_FULL,
            ResetKind::Full,
            &["session", "weekly"],
            1,
        ),
        (
            fixtures::CONFIRM_SESSION_2_1_312,
            ResetKind::Session,
            &["5-hour"],
            2,
        ),
        (
            fixtures::CONFIRM_FULL_2_1_312,
            ResetKind::Full,
            &["5-hour", "weekly"],
            1,
        ),
    ] {
        let ConfirmRead::Exact(read) = confirm_read(&screen(fixture)) else {
            panic!("{kind:?}: {:?}", confirm_read(&screen(fixture)));
        };
        assert_eq!(
            (
                read.kind,
                read.limits.iter().map(String::as_str).collect::<Vec<_>>(),
                read.left,
                read.week.as_str(),
                read.deadline.as_str(),
                (read.title_row, read.yes_row, read.yes_focused)
            ),
            (
                kind,
                limits.to_vec(),
                left,
                "Oct 9 at 5am",
                "Oct 5 at 3pm",
                (3, 8, true)
            )
        );
    }
    for (body, detail, limits, week, left, deadline) in [
        (
            "Refills your weekly limit now · your weekly reset day stays Oct 9 at 5:30am",
            "1 weekly reset left · use by Oct 5 at 12pm",
            &["weekly"][..],
            "Oct 9 at 5:30am",
            1,
            "Oct 5 at 12pm",
        ),
        (
            "Refills your weekly and Opus limits now · your weekly reset day stays Jan 2, \
             2027 at 9am",
            "12 weekly resets left · use by Dec 31 at 11:59pm",
            &["weekly", "Opus"],
            "Jan 2, 2027 at 9am",
            12,
            "Dec 31 at 11:59pm",
        ),
        (
            "Refills your weekly, Opus and Sonnet limits now · your weekly reset day stays \
             Oct 9 at 5am",
            "2 weekly resets left · use by Jan 1, 2027 at 1am",
            &["weekly", "Opus", "Sonnet"],
            "Oct 9 at 5am",
            2,
            "Jan 1, 2027 at 1am",
        ),
    ] {
        let ConfirmRead::Exact(read) = confirm_read(&confirm_with(&[body, detail])) else {
            panic!("{body} / {detail}");
        };
        assert_eq!(
            (
                read.limits.iter().map(String::as_str).collect::<Vec<_>>(),
                read.week.as_str()
            ),
            (limits.to_vec(), week)
        );
        assert_eq!((read.left, read.deadline.as_str()), (left, deadline));
    }

    for (name, text) in fixtures::ALL {
        let screen = screen(text);
        let read = parse_prompt_v2(&screen).map(|p| reset_confirm(&p, &screen));
        let confirmation = name.starts_with("confirm");
        assert_eq!(
            read.is_some_and(|read| read != ConfirmRead::Not),
            confirmation,
            "{name}"
        );
    }
    for wall in [LIMIT_OPTIONS_DIALOG, LIMIT_OPTIONS_PANEL] {
        assert_eq!(confirm_read(&screen(wall)), ConfirmRead::Not);
    }
}

/// The confirmation of a reset of `kind` with `body` over a detail counting `left` of it.
fn confirm_kind(kind: ResetKind, body: &str, left: u32) -> Vec<String> {
    let detail = format!("{} left · use by Oct 5 at 3pm", kind.resets(left));
    confirm_of(&kind.title(), &[body, &detail], &kind.yes(), "No, keep it")
}

fn refills_body(limits: &str) -> String {
    format!("Refills your {limits} now · your weekly reset day stays Oct 9 at 5am")
}

#[test]
fn every_limit_type_maps_to_a_name_the_confirmation_reads_back() {
    let ConfirmRead::Exact(composed) = confirm_read(&screen(fixtures::CONFIRM)) else {
        panic!("the composed confirmation reads exactly");
    };
    let read_back = |kind: ResetKind, limits: &str| {
        let body = refills_body(limits);
        match confirm_read(&confirm_kind(kind, &body, 2)) {
            ConfirmRead::Exact(read) => read.limits,
            other => panic!("{kind:?} {body}: {other:?}"),
        }
    };
    for (limit_type, drawn) in [
        ("five_hour", &["5-hour", "session"][..]),
        ("seven_day", &["weekly"]),
        ("seven_day_overage_included", &["weekly"]),
        ("seven_day_opus", &["Opus"]),
        ("seven_day_sonnet", &["Sonnet"]),
    ] {
        assert_eq!(reset_limit_names(limit_type), drawn, "{limit_type}");
        assert_eq!(reset_limit_name(limit_type), drawn.first().copied());
        let kind = if limit_type == "five_hour" {
            ResetKind::Session
        } else {
            ResetKind::Weekly
        };
        for name in drawn {
            let limits = format!("{name} limit");
            assert_eq!(read_back(kind, &limits), [*name], "{limit_type}: {name}");
        }
    }
    for limit_type in ["seven_day", "seven_day_overage_included"] {
        assert_eq!(
            reset_limit_name(limit_type).map(|name| vec![name.to_string()]),
            Some(composed.limits.clone()),
            "{limit_type}"
        );
    }
    for limit_type in ["limit", "overage", "", "seven_day_haiku", "Five_hour"] {
        assert_eq!(reset_limit_name(limit_type), None, "{limit_type:?}");
        assert!(reset_limit_names(limit_type).is_empty(), "{limit_type:?}");
    }
}

/// A confirmation reads exactly only where its body names what its kind refills, by the
/// fallback of the vendor's reading of a grant's kind from its clears (`qe()`), on 2.1.311's
/// words (`session`) or 2.1.312's (`5-hour`), never both, each name one a build draws for a
/// window a takeable kind refills (`5-hour`, `session`, `weekly`, `Opus`, `Sonnet`); every near
/// miss reads `body`: another spelling, a name no build draws for such a window (`Fable`,
/// `Weekly`, `Session`), or both builds' words.
#[test]
fn a_confirmation_names_exactly_what_its_kind_refills_on_either_builds_words() {
    use ResetKind::{Full, Session, Weekly};
    let exact = |kind: ResetKind, limits: &str| match confirm_read(&confirm_kind(
        kind,
        &refills_body(limits),
        1,
    )) {
        ConfirmRead::Exact(read) => Some(read.limits),
        ConfirmRead::Inexact("body") => None,
        other => panic!("{kind:?} {limits}: {other:?}"),
    };
    for (kind, limits, names) in [
        (Session, "5-hour limit", &["5-hour"][..]),
        (Session, "session limit", &["session"]),
        (Full, "5-hour and weekly limits", &["5-hour", "weekly"]),
        (Full, "session and weekly limits", &["session", "weekly"]),
        (
            Full,
            "5-hour, weekly and Opus limits",
            &["5-hour", "weekly", "Opus"],
        ),
        (
            Full,
            "session, weekly and Opus limits",
            &["session", "weekly", "Opus"],
        ),
        (
            Full,
            "5-hour, weekly, Opus and Sonnet limits",
            &["5-hour", "weekly", "Opus", "Sonnet"],
        ),
        (Weekly, "weekly limit", &["weekly"]),
        (Weekly, "weekly and Opus limits", &["weekly", "Opus"]),
        (Weekly, "Opus limit", &["Opus"]),
        (Weekly, "Sonnet and weekly limits", &["Sonnet", "weekly"]),
    ] {
        assert_eq!(
            exact(kind, limits),
            Some(names.iter().map(|n| (*n).to_string()).collect()),
            "{kind:?}: {limits}"
        );
    }
    for (kind, limits) in [
        (Session, "5-hour and weekly limits"),
        (Session, "session and weekly limits"),
        (Session, "5-hour and Opus limits"),
        (Session, "weekly limit"),
        (Session, "5-Hour limit"),
        (Session, "five-hour limit"),
        (Session, "5 hour limit"),
        (Session, "5-hours limit"),
        (Session, "5\u{2013}hour limit"),
        (Session, "5-hour session limit"),
        (Session, "session 5-hour limit"),
        (Session, "session and 5-hour limits"),
        (Session, "5-hour and session limits"),
        (Session, "5-hour and 5-hour limits"),
        (Session, "Session limit"),
        (Full, "5-hour limit"),
        (Full, "session limit"),
        (Full, "weekly limit"),
        (Full, "5-Hour and weekly limits"),
        (Full, "five-hour and weekly limits"),
        (Full, "5-hour, session and weekly limits"),
        (Full, "session, 5-hour and weekly limits"),
        (Full, "5-hour, weekly and weekly limits"),
        (Full, "5-hour and Weekly limits"),
        (Full, "session and Weekly limits"),
        (Full, "5-hour and Fable limits"),
        (Full, "Fable and 5-hour limits"),
        (Full, "session and Fable limits"),
        (Full, "5-hour and limit limits"),
        (Full, "5-hour, Session and weekly limits"),
        (Full, "5-hour and opus limits"),
        (Weekly, "5-hour limit"),
        (Weekly, "session limit"),
        (Weekly, "5-hour and weekly limits"),
        (Weekly, "session and weekly limits"),
        (Weekly, "weekly and weekly limits"),
        (Weekly, "Weekly limit"),
        (Weekly, "Fable limit"),
        (Weekly, "weekly and Fable limits"),
        (Weekly, "weekly and Session limits"),
        (Weekly, "weekly and sonnet limits"),
        (Weekly, "usage credit limit"),
    ] {
        assert_eq!(exact(kind, limits), None, "{kind:?}: {limits}");
    }
    for (fixture, older) in [
        (fixtures::CONFIRM_SESSION_2_1_312, fixtures::CONFIRM_SESSION),
        (fixtures::CONFIRM_FULL_2_1_312, fixtures::CONFIRM_FULL),
    ] {
        let rows = screen(fixture);
        let ConfirmRead::Exact(read) = confirm_read(&rows) else {
            panic!("{:?}", confirm_read(&rows));
        };
        let as_older: Vec<String> = rows
            .iter()
            .map(|row| row.replace("your 5-hour", "your session"))
            .collect();
        assert_eq!(
            as_older,
            screen(older),
            "the same screen in 2.1.311's words"
        );
        for near in ["5-Hour", "five-hour", "session 5-hour", "5-hour session"] {
            let edited: Vec<String> = rows
                .iter()
                .map(|row| row.replacen("your 5-hour", &format!("your {near}"), 1))
                .collect();
            assert_eq!(
                confirm_read(&edited),
                ConfirmRead::Inexact("body"),
                "{:?}: {near}",
                read.kind
            );
        }
    }
}

#[test]
fn a_confirmation_not_read_exactly_names_what_did_not_read() {
    const EARLY: &str =
        "You still have 40% of your weekly limit left — use your weekly reset anyway?";
    for (why, body) in [
        ("early", &[EARLY, BODY, DETAIL][..]),
        (
            "early",
            &[
                "You haven't reached a limit yet — use your weekly reset anyway?",
                BODY,
                DETAIL,
            ],
        ),
        (
            "early",
            &[
                "You haven't reached a limit yet — use your 5-hour reset anyway?",
                BODY,
                DETAIL,
            ],
        ),
        (
            "early",
            &[
                "You still have 40% of your weekly limit left — use your weekly",
                "reset anyway?",
                BODY,
                DETAIL,
            ],
        ),
        (
            "body",
            &[
                "Refill your weekly limit now · your weekly reset day stays Oct 9 at 5am",
                DETAIL,
            ],
        ),
        (
            "body",
            &[
                "Refills your limits now · your weekly reset day stays Oct 9 at 5am",
                DETAIL,
            ],
        ),
        (
            "body",
            &[
                "Refills your weekly limit nowhere · your weekly reset day stays Oct 9 at 5am",
                DETAIL,
            ],
        ),
        (
            "body",
            &[
                "Refills your  limit now · your weekly reset day stays Oct 9 at 5am",
                DETAIL,
            ],
        ),
        (
            "body",
            &[
                "Refills your weekly, limits now · your weekly reset day stays Oct 9 at 5am",
                DETAIL,
            ],
        ),
        ("body", &["Heads up: this cannot be undone", BODY, DETAIL]),
        ("body", &[BODY, DETAIL, "This cannot be undone"]),
        ("body", &[DETAIL, BODY]),
        ("body", &[]),
        ("week", &["Refills your weekly limit now", DETAIL]),
        (
            "week",
            &[
                "Refills your weekly limit now · your weekly reset day moves to Oct 9 at 5am",
                DETAIL,
            ],
        ),
        (
            "week",
            &[
                "Refills your weekly limit now · your weekly reset day stays Oct 9",
                DETAIL,
            ],
        ),
        (
            "week",
            &[
                "Refills your weekly limit now · your weekly reset day stays …",
                DETAIL,
            ],
        ),
        (
            "week",
            &[
                "Refills your weekly limit now · your weekly reset day stays Oct 9 at 13pm",
                DETAIL,
            ],
        ),
        (
            "week",
            &[
                "Refills your weekly limit now · your weekly reset day stays Oct 9 at 5:3am",
                DETAIL,
            ],
        ),
        (
            "week",
            &[
                "Refills your weekly limit now · returns what Fable used to your weekly limit · \
                 your weekly reset day stays Oct 9 at 5am",
                DETAIL,
            ],
        ),
        (
            "week",
            &[
                "Refills your weekly limit now · your weekly reset day stays Oct 9 at 5am (UTC)",
                DETAIL,
            ],
        ),
        ("count", &[BODY, "weekly resets left · use by Oct 5 at 3pm"]),
        (
            "count",
            &[BODY, "no weekly resets left · use by Oct 5 at 3pm"],
        ),
        (
            "count",
            &[BODY, "0 weekly resets left · use by Oct 5 at 3pm"],
        ),
        (
            "count",
            &[BODY, "1 weekly resets left · use by Oct 5 at 3pm"],
        ),
        (
            "count",
            &[BODY, "2 weekly reset left · use by Oct 5 at 3pm"],
        ),
        (
            "count",
            &[BODY, "2 five-hour resets left · use by Oct 5 at 3pm"],
        ),
        ("count", &[BODY, "2 full resets left · use by Oct 5 at 3pm"]),
        ("count", &[BODY, OLD_DETAIL]),
        ("count", &[BODY, "… left · use by …"]),
        ("count", &[BODY, "use by Oct 5 at 3pm"]),
        ("count", &[BODY]),
        ("deadline", &[BODY, "2 weekly resets left · use by Oct 5"]),
        ("deadline", &[BODY, "2 weekly resets left"]),
        (
            "deadline",
            &[BODY, "2 weekly resets left · expires Oct 5 at 3pm"],
        ),
        (
            "deadline",
            &[BODY, "2 weekly resets left · use by tomorrow at 3pm"],
        ),
        (
            "deadline",
            &[BODY, "2 weekly resets left · use by Oct 5 at 3 PM"],
        ),
        (
            "deadline",
            &[BODY, "2 weekly resets left · use by Oct 5 at 3pm (UTC)"],
        ),
    ] {
        assert_eq!(
            confirm_read(&confirm_with(body)),
            ConfirmRead::Inexact(why),
            "{body:?}"
        );
    }
    assert_eq!(
        confirm_read(&screen(fixtures::CONFIRM_EARLY)),
        ConfirmRead::Inexact("early")
    );
    assert_eq!(
        confirm_read(&confirm_of(
            "Use your reset?",
            &[BODY, OLD_DETAIL],
            "Yes, use my reset",
            "No, keep it",
        )),
        ConfirmRead::Inexact("title")
    );
    assert_eq!(
        confirm_read(&screen(fixtures::CONFIRM_FABLE)),
        ConfirmRead::Inexact("kind")
    );

    let exact = screen(fixtures::CONFIRM);
    let options = |yes: &str, no: &str, more: &[&str]| {
        let mut rows = exact[..8].to_vec();
        rows.push(format!("   {yes}"));
        rows.push(format!("   {no}"));
        rows.extend(more.iter().map(|row| format!("   {row}")));
        rows.extend(exact[10..].iter().cloned());
        rows
    };
    assert_eq!(
        options("❯ Yes, use my weekly reset", "  No, keep it", &[]),
        exact
    );
    for rows in [
        options("❯ 1. Yes, use my weekly reset", "  2. No, keep it", &[]),
        options(
            "❯ Yes, use my weekly reset",
            "  No, keep it",
            &["  Ask me later"],
        ),
        options("  No, keep it", "❯ Yes, use my weekly reset", &[]),
        options("❯ Yes, use my weekly reset", "  No", &[]),
        options("❯ Yes, use my weekly reset now", "  No, keep it", &[]),
        options(
            "❯ Yes, use my weekly reset",
            "  Yes, use my weekly reset",
            &[],
        ),
        options("❯ Yes, use my 5-hour reset", "  No, keep it", &[]),
        options("❯ Yes, use my full reset", "  No, keep it", &[]),
        options("❯ Yes, use my reset", "  No, keep it", &[]),
    ] {
        assert_eq!(
            confirm_read(&rows),
            ConfirmRead::Inexact("options"),
            "{rows:#?}"
        );
    }
    let p = boxed(&exact);
    for focused in [(true, true), (false, false)] {
        let mut edited = p.clone();
        (edited.options[0].focused, edited.options[1].focused) = focused;
        assert_eq!(
            reset_confirm(&edited, &exact),
            ConfirmRead::Inexact("options")
        );
    }

    for title in [
        "   Spend your reset?",
        "   Use your reset?",
        "   Use a weekly reset",
    ] {
        let mut retitled = exact.clone();
        retitled[3] = title.to_string();
        assert_eq!(
            confirm_read(&retitled),
            ConfirmRead::Inexact("title"),
            "{title}"
        );
        assert_eq!(reset_confirm(&p, &retitled), ConfirmRead::Inexact("title"));
    }
    let mut cut = p.clone();
    cut.head_off_screen = true;
    assert_eq!(reset_confirm(&cut, &exact), ConfirmRead::Inexact("title"));
    assert_eq!(
        reset_confirm(&p, &exact[..3]),
        ConfirmRead::Inexact("title")
    );
}

#[test]
fn a_confirmation_is_told_by_any_part_left_of_it() {
    const QUESTION: &str = "Spend it now?";
    const PLAIN: &str = "This takes effect at once";
    let exact = screen(fixtures::CONFIRM);
    let boxed_as = |title: &str, body: &[&str], yes: &str, no: &str, cut: bool| {
        let rows = confirm_of(title, body, yes, no);
        if cut { rows[5..].to_vec() } else { rows }
    };
    assert_eq!(
        boxed_as(
            "Use a weekly reset?",
            &[BODY, DETAIL],
            "Yes, use my weekly reset",
            "No, keep it",
            false
        ),
        exact
    );
    for cut in [false, true] {
        for (what, body, no) in [
            ("its no", &[PLAIN][..], "No, keep it"),
            ("the body's head", &["Refills your tokens today"][..], "No"),
            (
                "the week's join",
                &["All yours again · your weekly reset day stays Oct 9 at 5am"][..],
                "No",
            ),
            (
                "the detail's join",
                &["2 weekly resets left · use by Oct 5 at 3pm"][..],
                "No",
            ),
            (
                "the early line",
                &["You haven't reached a limit yet — use your weekly reset anyway?"][..],
                "No",
            ),
            (
                "another kind's early line",
                &["You haven't reached a limit yet — use your full reset anyway?"][..],
                "No",
            ),
            ("the whole body", &[BODY, DETAIL][..], "Not now"),
        ] {
            let rows = boxed_as(QUESTION, body, "Yes, refill now", no, cut);
            let p = boxed(&rows);
            assert_eq!(p.head_off_screen, cut, "{what}");
            assert_eq!(
                reset_confirm(&p, &rows),
                ConfirmRead::Inexact("title"),
                "{what}, cut {cut}: {rows:#?}"
            );
            let rows = boxed_as(QUESTION, body, "Yes", no, cut);
            assert_eq!(
                confirm_read(&rows),
                ConfirmRead::Inexact("title"),
                "{what}, cut {cut}, a plain yes"
            );
        }
        let rows = boxed_as(QUESTION, &[PLAIN], "Yes, use my 5-hour reset", "No", cut);
        assert_eq!(
            confirm_read(&rows),
            ConfirmRead::Inexact("title"),
            "cut {cut}"
        );
        let other = boxed_as(QUESTION, &[PLAIN], "Yes", "No", cut);
        assert_eq!(confirm_read(&other), ConfirmRead::Not, "cut {cut}");
    }
    let rows = boxed_as(QUESTION, &[BODY, DETAIL], "Yes", "No, keep it", false);
    let mut numbered = boxed(&rows);
    assert_eq!(
        reset_confirm(&numbered, &rows),
        ConfirmRead::Inexact("title"),
        "unnumbered, it is"
    );
    (numbered.options[0].n, numbered.options[1].n) = (Some(1), Some(2));
    assert_eq!(reset_confirm(&numbered, &rows), ConfirmRead::Not);
    numbered.options[1].n = None;
    assert_eq!(reset_confirm(&numbered, &rows), ConfirmRead::Not);
    for wall in [LIMIT_OPTIONS_DIALOG, LIMIT_OPTIONS_PANEL] {
        assert_eq!(confirm_read(&screen(wall)), ConfirmRead::Not);
    }
    for dialog in [
        fixtures::DIALOG_GRANT,
        fixtures::DIALOG_KINDS,
        fixtures::DIALOG_FABLE,
        fixtures::DIALOG_WEEKLY,
        fixtures::DIALOG_WEEKLY_2_1_312,
        fixtures::DIALOG_WEEKLY_BESIDE_GRANT_2_1_312,
    ] {
        assert_eq!(confirm_read(&screen(dialog)), ConfirmRead::Not);
    }
}

#[test]
fn the_chooser_is_neither_the_dialog_nor_the_confirmation() {
    let base = screen(fixtures::CONFIRM);
    let mut rows = base[..3].to_vec();
    rows.push("   Which reset do you want to use?".to_string());
    rows.push(String::new());
    rows.push(format!("   ❯ {SESSION}"));
    rows.push(format!("     {GRANT}"));
    rows.push("     Full reset · can't be used while you're at your session limit".to_string());
    rows.extend(base[10..].iter().cloned());
    let p = boxed(&rows);
    assert_ne!(p.kind, PromptKind::UsageLimit);
    assert_eq!(p.options.len(), 3, "{:?}", p.options);
    assert_eq!(reset_row(&p, &[]), None);
    assert_eq!(reset_confirm(&p, &rows), ConfirmRead::Not);
    assert!(p.options[..2].iter().all(|o| spends_reset(&o.label)));
    assert_eq!(reset_pending(&rows), None);
    assert_eq!(
        row_among(&[STOP, SESSION, WAIT]).map(|r| r.kind),
        Some(ResetKind::Session)
    );
}

#[test]
fn a_pending_line_is_read_under_its_own_title_and_nowhere_else() {
    let checking = screen(fixtures::CHECKING);
    let resetting = screen(fixtures::RESETTING);
    assert_eq!(reset_pending(&checking), Some((ResetPending::Checking, 5)));
    assert_eq!(checking[5].trim(), "Checking for a reset…");
    assert_eq!(
        reset_pending(&resetting),
        Some((ResetPending::Resetting, 5))
    );
    assert_eq!(resetting[3].trim(), "Use a weekly reset?");
    assert!(
        resetting[5]
            .trim()
            .starts_with("Resetting your limits… this usually")
    );
    for kind in ResetKind::ALL {
        let mut titled = resetting.clone();
        titled[3] = format!("   {}", kind.title());
        assert_eq!(
            reset_pending(&titled),
            Some((ResetPending::Resetting, 5)),
            "{kind:?}"
        );
    }
    assert_eq!(
        reset_pending(&inline(&checking)),
        Some((ResetPending::Checking, 5))
    );
    assert_eq!(
        reset_pending(&inline(&resetting)),
        Some((ResetPending::Resetting, 5))
    );
    assert_eq!(
        reset_pending(&checking[..6]),
        Some((ResetPending::Checking, 5))
    );
    let mut wrapped = resetting[..5].to_vec();
    wrapped.push("   Resetting your limits… this usually takes a few".to_string());
    wrapped.push("   seconds".to_string());
    assert_eq!(reset_pending(&wrapped), Some((ResetPending::Resetting, 5)));
    for rows in [&checking, &resetting] {
        let reading = crate::read(Some("claude"), rows, None);
        assert_eq!(
            (
                reading.phase,
                reading.phase_authoritative,
                reading.composer.is_some(),
                reading.prompt.is_some()
            ),
            (crate::Phase::Idle, false, false, false)
        );
    }

    for (name, text) in fixtures::ALL {
        let pending = matches!(name, "checking" | "resetting");
        assert_eq!(reset_pending(&screen(text)).is_some(), pending, "{name}");
    }
    for wall in [LIMIT_OPTIONS_DIALOG, LIMIT_OPTIONS_PANEL] {
        assert_eq!(reset_pending(&screen(wall)), None);
    }
    let edited = |base: &[String], edit: &dyn Fn(&mut Vec<String>)| {
        let mut rows = base.to_vec();
        edit(&mut rows);
        reset_pending(&rows)
    };
    assert_eq!(
        edited(&resetting, &|r| r[3] = "   Use your reset?".to_string()),
        None,
        "the 2.1.287 title"
    );
    for base in [&checking, &resetting] {
        let other = if base == &checking {
            &resetting
        } else {
            &checking
        };
        assert_eq!(
            edited(base, &|r| r[3] = other[3].clone()),
            None,
            "the other state's title"
        );
        assert_eq!(edited(base, &|r| r[2] = String::new()), None, "no rule");
        assert_eq!(
            edited(base, &|r| r[3] = format!("{} now", r[3])),
            None,
            "a title run on"
        );
        assert_eq!(
            edited(base, &|r| r[5] = "   Looking for a reset…".to_string()),
            None,
            "another line"
        );
        assert_eq!(
            edited(base, &|r| r.insert(
                5,
                "   ❯ 1. Stop and wait for limit to reset".to_string()
            )),
            None,
            "an option over the line"
        );
        assert_eq!(
            edited(base, &|r| r.push("host:work user$ ".to_string())),
            None,
            "a shell line under it"
        );
        assert_eq!(
            edited(base, &|r| {
                r.splice(6..6, [String::new(), "   Something else".to_string()]);
            }),
            None,
            "a row of another block under the line"
        );
        assert_eq!(
            edited(base, &|r| r[5] = r[5][1..].to_string()),
            None,
            "the line one column out"
        );
        assert_eq!(
            edited(base, &|r| r
                .extend(crate::prompt::fixtures::composer("  ? for shortcuts"))),
            None,
            "the composer under it"
        );
    }
    for base in [&checking, &resetting] {
        let rule = "─".repeat(60);
        let framed: Vec<String> = [rule.clone(), "❯ ".to_string(), rule]
            .into_iter()
            .chain(base.iter().cloned())
            .collect();
        assert!(has_composer_frame(&framed));
        assert_eq!(reset_pending(&framed), None, "the composer is drawn");
        assert!(
            reset_pending(&framed[3..]).is_some(),
            "the same rows without it"
        );
    }
    let mut quoted = vec![
        "⏺ It printed this:".to_string(),
        "  What do you want to do?".to_string(),
        "  Checking for a reset…".to_string(),
        String::new(),
    ];
    assert_eq!(reset_pending(&quoted), None, "no rule over the title");
    quoted.extend(crate::prompt::fixtures::composer("  ? for shortcuts"));
    assert_eq!(reset_pending(&quoted), None);
    assert_eq!(reset_pending(&[]), None);
}

const FAILURES: [&str; 43] = [
    "That reset was already used · nothing changed just now · 1 weekly reset left",
    "That reset was already used · nothing changed just now · 1 five-hour reset left · run \
     /limit-reset again to use one",
    "This reset isn't available any more · nothing was used · this limit resets Oct 9 at 5am",
    "This reset isn't available any more · your earlier try may have gone through · checking \
     your limits now",
    "A different reset is on offer now · nothing was used · run /limit-reset again to see it",
    "Your weekly reset is no longer on offer · nothing was used · you can use a 5-hour reset",
    "Another reset was just started on your account · checking your limits now · if you're \
     still at the limit in a minute, try again",
    "Your earlier reset may still be going through · checking your limits now · try again \
     in a minute if they aren't refilled",
    "Your earlier reset went through after all · checking your limits now",
    "Couldn't reset your limits · nothing was used · try again in a moment",
    "Couldn't confirm the reset went through · if you're still at the limit in a moment, try \
     again",
    "Still couldn't confirm your reset · nothing more was used · if your limits aren't \
     refilled in a few minutes, contact support",
    "Your limits are already being reset · one moment",
    "No reset to use · you're not at a usage limit right now",
    "No 5-hour reset to use · you can use a weekly reset",
    "Your reset can't be used for this limit right now · this limit resets Oct 9 at 5am",
    "None of your resets refill your session limit · this limit resets 3pm",
    "Your reset doesn't refill your session limit, so it can't be used until that resets · \
     this limit resets Oct 9 at 5am",
    "This limit may have lifted already · checking your limits now · run /limit-reset again \
     in a moment to use your reset anyway",
    "Reset used · no resets left",
    "Couldn't check for a reset right now · try again in a moment",
    "Resets can't be used while you continue at lower priority · run /limit-reset after that \
     ends",
    "Resets kept · run /limit-reset when you want one · first expires Oct 5 at 3pm",
    "No reset to use · this limit resets Oct 9 at 5am",
    "Your Fable reset can't be used right now · no Fable usage this week to give back · this \
     limit resets Oct 9 at 5am",
    "Your reset can't be used for this limit right now · it is paused · this limit resets \
     Oct 9 at 5am",
    "Your reset can't be used for this limit right now · try again in a minute · this limit \
     resets 3pm",
    "This reset isn't available any more · nothing was used · you're not at a usage limit \
     right now",
    "Your weekly reset can't be used while you're at your session limit · you can use a \
     5-hour reset",
    "Your full reset can't be used while you're at your Opus limit · you can use a weekly \
     reset",
    "Your 5-hour reset can't be used while you're at another limit it doesn't refill · you \
     can use a weekly reset or a full reset",
    "Your weekly reset can't be used right now · you can use a 5-hour reset",
    "Your weekly reset can't be used right now · doesn't refill the limit you've reached · \
     you can use a 5-hour reset, a full reset or a Fable reset",
    "Your weekly reset can't be used right now · doesn't refill your session limit · you can \
     use a 5-hour reset",
    "Your full reset can't be used right now · usable once you reach a limit it refills · you \
     can use a 5-hour reset",
    "Your Fable reset can't be used right now · no Fable usage this week to give back · you \
     can use a weekly reset",
    "Your weekly reset can't be used right now · try again in a minute · you can use a 5-hour \
     reset",
    "You have a reset saved for when you reach a usage limit · run /limit-reset when you do · \
     1 reset left · use by Oct 5",
    "Your reset can't be used right now · it is paused · 2 resets left · use by Oct 5",
    "Your reset can't be used right now · try again later · 1 reset left · use by Oct 5",
    "Your reset only works at a usage limit, and you're not at one now · nothing was used",
    "Another reset was just started on your account · this one wasn't used · try again in a \
     minute",
    "Couldn't confirm the reset went through · check /usage in a moment · if your limits \
     weren't refilled, run /limit-reset again",
];
const SPENT: &str =
    "Weekly limits reset · your weekly reset day stays Oct 9 at 5am · 1 weekly reset left";
const LIFTED: [&str; 2] = [
    "Your limit had already reset on its own · nothing was used",
    "Your limits are clear",
];
const KEPT: &str =
    "Weekly reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm";
const LOGIN: &str = "Couldn't reset your limits with this login · run /login, then try again";
const JUNIPER: [&str; 7] = [
    "Session limit reset · next reset available Oct 9 at 5am · your weekly limit still applies",
    "Session limit reset · your weekly limit still applies",
    "Weekly reset used · available again Oct 9 at 5am",
    "Couldn't reset your session limit right now · try again in a moment",
    "A session-limit reset isn't available right now.",
    "Your session limit is already being reset · one moment",
    "Couldn't reset your session limit with this login · run /login, then try again",
];
/// [`JUNIPER`] as 2.1.312 draws it (`OJ` @228882669, `5-hour limit` for `session limit`), with
/// the row, its notice and its pending line.
const JUNIPER_2_1_312: [&str; 10] = [
    "5-hour limit reset · next reset available Oct 9 at 5am · your weekly limit still applies",
    "5-hour limit reset · your weekly limit still applies",
    "Couldn't reset your 5-hour limit right now · try again in a moment",
    "A 5-hour limit reset isn't available right now.",
    "Your 5-hour limit is already being reset · one moment",
    "Couldn't reset your 5-hour limit with this login · run /login, then try again",
    "Reset your 5-hour limit now",
    "Reset your 5-hour limit now and keep working; once a week, still counts toward your weekly \
     limit",
    "/limit-reset to reset your 5-hour limit now · uses weekly limit · 1/week",
    "Resetting your 5-hour limit… this usually takes a few seconds",
];

#[test]
fn a_spent_line_is_read_by_its_kinds_head() {
    let asking = |line: &str| format!("{line}{ASKS}");
    for (line, kind, left) in [
        (SPENT, ResetKind::Weekly, 1),
        (
            "Session limit reset · your weekly reset day stays Oct 9 at 5am · 1 five-hour \
             reset left",
            ResetKind::Session,
            1,
        ),
        (
            "Session limit reset · your weekly reset day stays Oct 9 at 5am · no five-hour \
             resets left",
            ResetKind::Session,
            0,
        ),
        (
            "Limits reset · your weekly reset day stays Jan 2, 2027 at 5:30am · 12 full \
             resets left",
            ResetKind::Full,
            12,
        ),
        (
            "Fable reset used · what Fable used is back in your weekly limit · your weekly \
             reset day stays Oct 9 at 5am · 1 Fable reset left",
            ResetKind::Fable,
            1,
        ),
        (
            "Session limit reset · 1 five-hour reset left",
            ResetKind::Session,
            1,
        ),
        (
            "5-hour limit reset · your weekly reset day stays Oct 9 at 5am · 1 five-hour reset \
             left",
            ResetKind::Session,
            1,
        ),
        (
            "5-hour limit reset · your weekly reset day stays Jan 2, 2027 at 5:30am · no \
             five-hour resets left",
            ResetKind::Session,
            0,
        ),
        (
            "5-hour limit reset · 2 five-hour resets left",
            ResetKind::Session,
            2,
        ),
        (
            "Weekly limits reset · no weekly resets left",
            ResetKind::Weekly,
            0,
        ),
        ("Limits reset · 3 full resets left", ResetKind::Full, 3),
    ] {
        assert_eq!(
            reset_line(line),
            ResetLine::Spent {
                kind,
                left: Some(left),
                asks: false
            },
            "{line}"
        );
        assert_eq!(
            reset_line(&asking(line)),
            ResetLine::Spent {
                kind,
                left: Some(left),
                asks: true
            },
            "{line}"
        );
    }
    for (line, kind, left) in [
        (
            "Limits reset · send a message to continue",
            ResetKind::Full,
            None,
        ),
        ("Limits reset · you have 3 more", ResetKind::Full, None),
        (
            "Weekly limits reset · you have 3 more",
            ResetKind::Weekly,
            None,
        ),
        (
            "Weekly limits reset · your weekly reset day stays Oct 9 at 5am",
            ResetKind::Weekly,
            None,
        ),
        (
            "Weekly limits reset · your weekly reset day stays Oct 9 · 1 weekly reset left",
            ResetKind::Weekly,
            Some(1),
        ),
        (
            "Weekly limits reset · 1 five-hour reset left",
            ResetKind::Weekly,
            None,
        ),
        (
            "Session limit reset · your weekly reset day stays Oct 9 at 5am",
            ResetKind::Session,
            None,
        ),
        (
            "Session limit reset · refilled · 2 five-hour resets left",
            ResetKind::Session,
            Some(2),
        ),
        (
            "5-hour limit reset · your weekly reset day stays Oct 9 at 5am",
            ResetKind::Session,
            None,
        ),
        (
            "Fable reset used · what Fable used is back in your weekly limit · enjoy",
            ResetKind::Fable,
            None,
        ),
        (
            "Limits reset · your weekly reset day stays Oct 9 at 5am · 1 reset left",
            ResetKind::Full,
            None,
        ),
        ("Limits reset · 3 resets left", ResetKind::Full, None),
    ] {
        let asks = line.ends_with(ASKS);
        assert_eq!(
            reset_line(line),
            ResetLine::Spent { kind, left, asks },
            "{line}"
        );
    }
    let walled = "Fable reset used · you're still at your weekly limit until Oct 9 at 5am · no \
                  Fable resets left";
    assert_eq!(
        reset_line(walled),
        ResetLine::Walled {
            kind: ResetKind::Fable,
            left: 0,
        }
    );
    assert_eq!(
        reset_line(&walled.replace("you're", "you’re")),
        reset_line(walled),
        "a typographic apostrophe"
    );
    assert_eq!(reset_line(&asking(walled)), ResetLine::Other);
    assert_eq!(
        reset_line(&format!("  {SPENT}\n")),
        ResetLine::Spent {
            kind: ResetKind::Weekly,
            left: Some(1),
            asks: false
        },
        "the row's own white space"
    );
    for line in LIFTED {
        assert_eq!(
            reset_line(line),
            ResetLine::Lifted { asks: false },
            "{line}"
        );
        assert_eq!(
            reset_line(&asking(line)),
            ResetLine::Lifted { asks: true },
            "{line}"
        );
    }
    for line in [
        KEPT,
        "Weekly reset kept · run /limit-reset whenever you want to use it · use by Oct 5 at \
         3pm",
        "5-hour reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm",
        "Full reset kept · run /limit-reset at a limit to use it",
        "Fable reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm",
    ] {
        assert_eq!(reset_line(line), ResetLine::Kept, "{line}");
    }
    assert_eq!(reset_line(LOGIN), ResetLine::Login);
    assert_eq!(
        reset_line("Couldn’t reset your limits with this login · run /login, then try again"),
        ResetLine::Login,
        "a typographic apostrophe"
    );
    for line in FAILURES.iter().chain(&JUNIPER).chain(&JUNIPER_2_1_312) {
        assert_eq!(reset_line(line), ResetLine::Other, "{line}");
        assert_eq!(reset_line(&asking(line)), ResetLine::Other, "{line}");
    }
    for line in [
        "Limits reset",
        "Weekly limits reset",
        "Limits reset on Oct 5",
        "Limits reset: 1 full reset left",
        "Limits resets · 1 full reset left",
        "limits reset · 1 full reset left",
        "WEEKLY LIMITS RESET · 1 weekly reset left",
        "Session limit reset · 1 weekly reset left",
        "Session limit reset · 1 5-hour reset left",
        "Session limit reset · send a message to continue",
        "Session limit reset · refilled",
        "5-hour limit reset",
        "5-hour limit reset · 1 weekly reset left",
        "5-hour limit reset · 1 5-hour reset left",
        "5-hour limit reset · send a message to continue",
        "5-hour limit resets · 1 five-hour reset left",
        "5-Hour limit reset · 1 five-hour reset left",
        "5-hour limit reset: 1 five-hour reset left",
        "The line was: 5-hour limit reset · 1 five-hour reset left",
        "> 5-hour limit reset · your weekly reset day stays Oct 9 at 5am",
        "The line was: Weekly limits reset · 1 weekly reset left",
        "Fable reset used · 1 Fable reset left",
        "Fable reset used · you're still at your weekly limit · 1 Fable reset left",
        "Your limits are clear · nothing was used",
        "Your limits are clear now",
        "Your limit had already reset on its own",
        "Weekly reset kept",
        "weekly reset kept · run /limit-reset at a limit to use it",
        "Weekly reset kept, run /limit-reset at a limit to use it",
        "Couldn't reset your limits",
        "Reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm",
        "Claude Code will continue automatically at 6am. Keep this session open; it may \
         still pause for permission prompts. Press esc to cancel the wait.",
        "send a message to continue",
        "",
    ] {
        assert_eq!(reset_line(line), ResetLine::Other, "{line:?}");
    }
}

#[test]
fn no_line_of_the_reset_program_is_a_wall() {
    use crate::wall::{WallKind, classify_wall};
    let fixed = [
        SPENT,
        "Weekly limits reset · no weekly resets left",
        "Session limit reset · your weekly reset day stays Oct 9 at 5am · 1 five-hour reset left",
        "Session limit reset · 1 five-hour reset left",
        "5-hour limit reset · your weekly reset day stays Oct 9 at 5am · 1 five-hour reset left",
        "5-hour limit reset · 1 five-hour reset left",
        "Limits reset · your weekly reset day stays Oct 9 at 5am · 2 full resets left",
        "Fable reset used · what Fable used is back in your weekly limit · your weekly reset \
         day stays Oct 9 at 5am · 1 Fable reset left",
        "Fable reset used · you're still at your weekly limit until Oct 9 at 5am · no Fable \
         resets left",
        LIFTED[0],
        LIFTED[1],
        KEPT,
        LOGIN,
        "Weekly reset kept · run /limit-reset whenever you want to use it · use by Oct 5 at 3pm",
        "5-hour reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm",
        "Full reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm",
        "Fable reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm",
        "Checking for a reset…",
        "Resetting your limits… this usually takes a few seconds",
        GRANT,
        SESSION,
        FULL,
        FABLE,
        FABLE_AT_WEEK,
        "Use a weekly reset",
        "Use a 5-hour reset?",
        "Use a weekly reset?",
        "Use a full reset?",
        "Use a Fable reset?",
        BODY,
        "Refills your session limit now · your weekly reset day stays Oct 9 at 5am",
        "Refills your session and weekly limits now · your weekly reset day stays Oct 9 at 5am",
        "Refills your Fable limit now · returns what Fable used to your weekly limit · your \
         weekly reset day stays Oct 9 at 5am",
        DETAIL,
        "2 five-hour resets left · use by Oct 5 at 3pm",
        "1 full reset left · use by Oct 5 at 3pm",
        "Yes, use my weekly reset",
        "Yes, use my 5-hour reset",
        "No, keep it",
        "You still have 40% of your weekly limit left — use your weekly reset anyway?",
        "You still have 40% of your session limit left — use your 5-hour reset anyway?",
        "You haven't reached a limit yet — use your full reset anyway?",
        "/limit-reset to use your 5-hour reset · 2 left · use by Oct 5",
        "/limit-reset to use your weekly reset · 1 left · use by Oct 9",
        "/limit-reset · 2 five-hour, 1 Fable resets · first expires Oct 5",
        "Reset used · 1 reset left",
        "Which reset do you want to use?",
        WEEKLY,
        "/limit-reset to reset your session limit now · uses weekly limit · 1/week",
        "Resetting your session limit… this usually takes a few seconds",
    ];
    let reasons = [
        "try again in a minute",
        "can't be used while you're at your session limit",
        "can't be used while you're at your weekly limit",
        "can't be used while you're at your Opus limit",
        "can't be used while you're at your Sonnet limit",
        "no Fable usage this week to give back",
        "can't be used while you're at another limit it doesn't refill",
        "doesn't refill the limit you've reached",
        "usable once you reach a limit it refills",
        "can't be used right now",
        "doesn't refill your session limit",
        "doesn't refill your weekly limit",
        "doesn't refill your Opus limit",
        "doesn't refill your Sonnet limit",
        "doesn't refill your Fable limit",
        "doesn't refill your usage credit limit",
    ];
    let chooser = ["5-hour reset", "Weekly reset", "Full reset", "Fable reset"]
        .into_iter()
        .flat_map(|kind| reasons.map(|reason| format!("{kind}{SEP}{reason}")));
    let lines = fixed
        .iter()
        .chain(&FAILURES)
        .chain(&JUNIPER)
        .chain(&JUNIPER_2_1_312);
    for line in lines.map(|line| (*line).to_string()).chain(chooser) {
        for line in [format!("{line}{ASKS}"), line] {
            assert_eq!(classify_wall(&line), None, "{line}");
        }
    }
    assert_eq!(
        classify_wall("You've hit your weekly limit · resets Oct 9 at 5am"),
        Some(WallKind::UsageWeekly)
    );
    assert_eq!(
        classify_wall("Weekly limit reached · resets Oct 9 at 5am"),
        Some(WallKind::UsageWeekly)
    );
    assert_eq!(
        classify_wall("Please run /login · API Error: 401"),
        Some(WallKind::Auth)
    );
    for wall in [
        "You've hit your session limit · resets 3pm",
        "You've hit your 5-hour limit · resets 3pm",
        "You've hit your 5-hour limit · resets 3pm · progress saved",
        "You’ve hit your 5-hour limit · resets 5:11pm (America/Los_Angeles)",
        "Session limit reached ∙ resets 5pm",
        "You've hit your limit · resets 7:30pm (America/Los_Angeles)",
    ] {
        assert_eq!(classify_wall(wall), Some(WallKind::UsageSession), "{wall}");
    }
}

#[test]
fn claude_2_1_312s_reset_answers_are_no_wall_and_its_wall_still_is() {
    use crate::phase::{Phase, worker_phase};
    use crate::wall::{WallKind, wall};
    let session = ResetLine::Spent {
        kind: ResetKind::Session,
        left: Some(1),
        asks: false,
    };
    for (name, text, read) in [
        (
            "weekly row",
            fixtures::DONE_WEEKLY_ROW_2_1_312,
            ResetLine::Other,
        ),
        ("5-hour kind", fixtures::SPENT_SESSION_2_1_312, session),
    ] {
        let rows = screen(text);
        assert_eq!(wall(&rows), None, "{name}");
        assert_eq!(worker_phase(&rows), Phase::Idle, "{name}");
        let said = rows
            .iter()
            .rev()
            .find_map(|row| row.trim_start().strip_prefix('⎿'))
            .expect("the answer row");
        assert_eq!(reset_line(said), read, "{name}");
        let command = rows
            .iter()
            .position(|row| row == "❯ /rate-limit-options")
            .expect("the command row");
        let frame = rows
            .iter()
            .position(|row| is_rule(row))
            .expect("the composer's rule");
        let walled: Vec<String> = rows[..command]
            .iter()
            .chain(&rows[frame..])
            .cloned()
            .collect();
        let w = wall(&walled).expect("the wall the answer came after");
        assert_eq!(w.kind, WallKind::UsageSession, "{name}: {w:?}");
        assert!(
            w.message
                .starts_with("You've hit your 5-hour limit · resets 5:11pm"),
            "{name}: {w:?}"
        );
    }
    let quoted = screen(fixtures::QUOTED_2_1_312);
    assert_eq!(wall(&quoted), None);
    assert_eq!(worker_phase(&quoted), Phase::Idle);
    assert_eq!(parse_prompt_v2(&quoted), None);
    assert_eq!(reset_pending(&quoted), None);
}

/// Every screen of the reset program is sighted as what it shows: each dialog with a reset row
/// as an offer, and the usage-limit dialog with no reset row on offer as a dialog (the 2.1.304
/// dialog MEASURED 2026-10-19, and the panel form). The rows from a sighting's `top` to the
/// screen's foot are sighted the same, with the same key, and the transcript above it is cut
/// (each fixture's first row of reply, banner or turn).
/// NEGATIVE CONTROLS: the worker's tool output and reply quoting 2.1.312's answer and row
/// (`QUOTED_2_1_312`, its `⎿` rows under a Bash call), a wall with the wait armed and no
/// dialog, a `/limit-reset` typed into the composer and not sent, a confirmation's title in a
/// tool's output (with the composer drawn and with it cut), and an answer a later reply was
/// drawn under are no sighting.
#[test]
fn each_screen_of_the_reset_program_is_sighted_as_what_it_shows() {
    let spent = ResetSight::Answer(ResetLine::Spent {
        kind: ResetKind::Session,
        left: Some(1),
        asks: false,
    });
    for (name, text) in fixtures::ALL {
        let want = match name {
            "quoted-2.1.312" => None,
            "checking" => Some(ResetSight::Pending(ResetPending::Checking)),
            "resetting" => Some(ResetSight::Pending(ResetPending::Resetting)),
            "done-weekly-row-2.1.312" => Some(ResetSight::Answer(ResetLine::Other)),
            "spent-session-2.1.312" => Some(spent),
            n if n.starts_with("dialog-") => Some(ResetSight::Offer),
            n if n.starts_with("confirm") => Some(ResetSight::Confirmation),
            other => panic!("no sight is written down for {other}"),
        };
        let rows = screen(text);
        let got = reset_sighting(&rows);
        assert_eq!(got.as_ref().map(|s| s.sight), want, "{name}: {got:?}");
        let Some(got) = got else { continue };
        assert!(got.top > 0, "{name}: the transcript over the sight is cut");
        assert_eq!(
            reset_sighting(&rows[got.top..]),
            Some(ResetSighting {
                top: 0,
                ..got.clone()
            }),
            "{name}"
        );
    }
    for (name, text) in [
        (
            "2.1.304 measured",
            include_str!("fixtures/claude-2.1.304-limit-dialog-no-reset-rows.txt"),
        ),
        ("dialog", LIMIT_OPTIONS_DIALOG),
        ("panel", LIMIT_OPTIONS_PANEL),
    ] {
        let got = reset_sighting(&screen(text)).map(|s| s.sight);
        assert_eq!(got, Some(ResetSight::Dialog), "{name}");
    }
    let armed = screen(include_str!(
        "fixtures/claude-2.1.296-wall-session-auto-continue-armed.txt"
    ));
    assert_eq!(reset_sighting(&armed), None);
    let mut typed = screen(fixtures::QUOTED_2_1_312);
    let composer = typed
        .iter()
        .rposition(|row| row.trim() == "❯")
        .expect("the composer row");
    typed[composer] = "❯ /limit-reset".to_string();
    assert_eq!(reset_sighting(&typed), None);
    let mut quoted = screen(fixtures::QUOTED_2_1_312);
    let output = quoted
        .iter()
        .position(|row| row.trim() == "Reset your 5-hour limit now")
        .expect("the tool's output row");
    quoted.insert(output + 1, "     Use a weekly reset?".to_string());
    assert_eq!(reset_sighting(&quoted), None);
    let bare: Vec<String> = quoted
        .iter()
        .take_while(|row| !row.starts_with("✻"))
        .cloned()
        .collect();
    assert_eq!(reset_sighting(&bare), None);
    let mut later = screen(fixtures::SPENT_SESSION_2_1_312);
    let answer = later
        .iter()
        .position(|row| row.trim_start().starts_with("⎿  5-hour limit reset"))
        .expect("the answer row");
    later.insert(answer + 2, "⏺ Back at work on the suites.".to_string());
    assert_eq!(reset_sighting(&later), None);
    later.remove(answer + 2);
    assert!(reset_sighting(&later).is_some());
}

/// A sighting's key is its own rows: the same dialog under more transcript has the same key,
/// and the cursor on another row, or another answer, gives another. The answer under a typed
/// `/limit-reset <kind>` is read with its wrapped rows. NEGATIVE CONTROL: an answer row with
/// no command row over it (the command row cut) is no sighting.
#[test]
fn a_sightings_key_is_its_own_rows() {
    let rows = screen(fixtures::DIALOG_KINDS);
    let first = reset_sighting(&rows).expect("the dialog");
    let mut more = rows.clone();
    more.insert(0, "⏺ An earlier reply.".to_string());
    assert_eq!(reset_sighting(&more).expect("the dialog").key, first.key);
    let moved: Vec<String> = rows
        .iter()
        .map(|row| {
            row.replace("   ❯ 1. Stop", "     1. Stop")
                .replace("     2. Use a 5-hour", "   ❯ 2. Use a 5-hour")
        })
        .collect();
    assert_ne!(moved, rows);
    let moved = reset_sighting(&moved).expect("the dialog");
    assert_eq!(moved.sight, ResetSight::Offer);
    assert_ne!(moved.key, first.key);
    let spent = screen(fixtures::SPENT_SESSION_2_1_312);
    let command = spent
        .iter()
        .position(|row| row == "❯ /rate-limit-options")
        .expect("the command row");
    let mut typed = spent.clone();
    typed[command] = "❯ /limit-reset weekly".to_string();
    typed[command + 1] = "  ⎿  Weekly limits reset · your weekly reset day stays Oct 9 at \
                          5am ·"
        .to_string();
    typed.insert(command + 2, "     1 weekly reset left".to_string());
    typed.remove(0);
    let got = reset_sighting(&typed).expect("the answer");
    assert_eq!(
        got.sight,
        ResetSight::Answer(ResetLine::Spent {
            kind: ResetKind::Weekly,
            left: Some(1),
            asks: false,
        }),
        "{got:?}"
    );
    assert!(
        got.key.starts_with("❯ /limit-reset weekly\n⎿  Weekly"),
        "{got:?}"
    );
    assert!(got.key.ends_with("\n1 weekly reset left"), "{got:?}");
    let spent_key = reset_sighting(&spent).expect("the answer").key;
    assert_ne!(got.key, spent_key);
    let mut cut = spent.clone();
    cut.remove(command);
    assert_eq!(reset_sighting(&cut), None);
}

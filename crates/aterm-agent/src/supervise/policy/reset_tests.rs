// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_phase::prompt::fixtures::screen;
use aterm_phase::reset::fixtures as composed;
use aterm_phase::{ResetConfirm, reset_confirm};

/// The confirmation on one of the reader's composed screens (never
/// captured: each fixture's first line says so), read as the loop reads it.
fn read(fixture: &str) -> ConfirmRead {
    let rows = screen(fixture);
    let reading = aterm_phase::read(Some("claude"), &rows, None);
    let prompt = reading.prompt.as_ref().expect("the confirmation is a box");
    reset_confirm(prompt, &rows)
}

fn exact(fixture: &str) -> ResetConfirm {
    match read(fixture) {
        ConfirmRead::Exact(confirm) => confirm,
        other => panic!("not exact: {other:?}"),
    }
}

/// The row the composed confirmation follows (`Use a weekly reset (2
/// left, until Oct 5)`), taken at a weekly wall that stands for days.
fn go() -> ResetGo {
    ResetAsk {
        kind: ResetKind::Weekly,
        left: 2,
        day: "Oct 5".to_string(),
        day_start: None,
    }
    .go(1_800_000_000, true, vec!["weekly".to_string()])
}

const NOW: i64 = 1_790_000_000;
const HOUR: i64 = 3600;

/// Claude Code 2.1.312's 5-hour and full confirmations (`5-hour` where 2.1.311 said
/// `session`) are entered at a five-hour wall the host names by every build's word
/// (`aterm_phase::reset_limit_names`), as 2.1.311's are; a host naming 2.1.311's word alone
/// keeps 2.1.312's 5-hour reset, and a near miss of 2.1.312's word never reads.
#[test]
fn claude_2_1_312s_confirmations_are_entered_by_either_builds_name_of_the_wall() {
    let five_hour: Vec<String> = aterm_phase::reset_limit_names("five_hour")
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    for (fixture, kind, left, limits) in [
        (
            composed::CONFIRM_SESSION_2_1_312,
            ResetKind::Session,
            2,
            &["5-hour"][..],
        ),
        (
            composed::CONFIRM_SESSION,
            ResetKind::Session,
            2,
            &["session"],
        ),
        (
            composed::CONFIRM_FULL_2_1_312,
            ResetKind::Full,
            1,
            &["5-hour", "weekly"],
        ),
        (
            composed::CONFIRM_FULL,
            ResetKind::Full,
            1,
            &["session", "weekly"],
        ),
    ] {
        assert_eq!(exact(fixture).limits, limits, "{kind:?}");
        let own = ResetGo {
            kind,
            left,
            limits: five_hour.clone(),
            ..go()
        };
        assert_eq!(
            confirm_verdict(&read(fixture), &own, None, NOW),
            Verdict::Enter,
            "{kind:?} {limits:?}"
        );
        let older = ResetGo {
            limits: vec!["session".to_string()],
            ..own.clone()
        };
        let by_older = if limits.contains(&"session") {
            Verdict::Enter
        } else {
            Verdict::Keep(KeepWhy::OtherLimit)
        };
        assert_eq!(
            confirm_verdict(&read(fixture), &older, None, NOW),
            by_older,
            "{kind:?} {limits:?} under 2.1.311's word alone"
        );
    }
    let rows = screen(composed::CONFIRM_SESSION_2_1_312);
    let own = ResetGo {
        kind: ResetKind::Session,
        left: 2,
        limits: five_hour,
        ..go()
    };
    for near in ["5-Hour", "five-hour", "session 5-hour", "5-hour session"] {
        let edited: Vec<String> = rows
            .iter()
            .map(|row| row.replacen("your 5-hour", &format!("your {near}"), 1))
            .collect();
        assert_ne!(edited, rows, "{near}");
        let reading = aterm_phase::read(Some("claude"), &edited, None);
        let prompt = reading.prompt.as_ref().expect("the confirmation is a box");
        assert_eq!(
            confirm_verdict(&reset_confirm(prompt, &edited), &own, None, NOW),
            Verdict::Keep(KeepWhy::Inexact("body")),
            "{near}"
        );
    }
}

/// Enter at the exact confirmation of the row taken, at a long wall,
/// whatever its deadline, for each kind the step takes; and each
/// [`KeepWhy`] the verdict can give, by one thing changed from that case
/// — a confirmation of another kind than the row's among them.
#[test]
fn enter_only_at_the_exact_confirmation_of_the_row_taken() {
    let confirm = exact(composed::CONFIRM);
    assert_eq!(
        (
            confirm.kind,
            confirm.limits.as_slice(),
            confirm.left,
            confirm.deadline.as_str()
        ),
        (
            ResetKind::Weekly,
            ["weekly".to_string()].as_slice(),
            2,
            "Oct 5 at 3pm"
        )
    );
    for (fixture, kind, left, limit) in [
        (composed::CONFIRM_SESSION, ResetKind::Session, 2, "session"),
        (composed::CONFIRM_FULL, ResetKind::Full, 1, "weekly"),
    ] {
        let own = ResetGo {
            kind,
            left,
            limits: vec![limit.to_string()],
            ..go()
        };
        assert_eq!(
            confirm_verdict(&read(fixture), &own, None, NOW),
            Verdict::Enter,
            "{kind:?}"
        );
        assert_eq!(
            confirm_verdict(
                &read(fixture),
                &ResetGo {
                    kind: ResetKind::Weekly,
                    ..own
                },
                None,
                NOW
            ),
            Verdict::Keep(KeepWhy::Kind),
            "{kind:?} after a weekly row"
        );
    }
    let read_ok = ConfirmRead::Exact(confirm.clone());
    for deadline in [None, Some(NOW - HOUR), Some(NOW + 300 * HOUR)] {
        assert_eq!(
            confirm_verdict(&read_ok, &go(), deadline, NOW),
            Verdict::Enter
        );
    }
    assert_eq!(
        confirm_verdict(&read(composed::CONFIRM_60), &go(), None, NOW),
        Verdict::Enter
    );
    let either = ResetGo {
        limits: vec!["Sonnet".to_string(), "weekly".to_string()],
        ..go()
    };
    assert_eq!(
        confirm_verdict(&read_ok, &either, None, NOW),
        Verdict::Enter
    );
    let both = ConfirmRead::Exact(ResetConfirm {
        limits: vec!["session".to_string(), "weekly".to_string()],
        ..confirm.clone()
    });
    assert_eq!(confirm_verdict(&both, &go(), None, NOW), Verdict::Enter);

    let with_go = |edit: &dyn Fn(&mut ResetGo)| {
        let mut go = go();
        edit(&mut go);
        go
    };
    let with_read = |edit: &dyn Fn(&mut ResetConfirm)| {
        let mut c = confirm.clone();
        edit(&mut c);
        ConfirmRead::Exact(c)
    };
    let rows: Vec<(&str, ConfirmRead, ResetGo, Option<i64>, KeepWhy)> = vec![
        (
            "the early line (the reader's composed screen)",
            read(composed::CONFIRM_EARLY),
            go(),
            None,
            KeepWhy::Inexact("early"),
        ),
        (
            "a title that did not read",
            ConfirmRead::Inexact("title"),
            go(),
            None,
            KeepWhy::Inexact("title"),
        ),
        (
            "no confirmation at all",
            ConfirmRead::Not,
            go(),
            None,
            KeepWhy::Closed,
        ),
        (
            "the Fable reset's confirmation (the reader's composed screen)",
            read(composed::CONFIRM_FABLE),
            go(),
            None,
            KeepWhy::Inexact("kind"),
        ),
        (
            "the row taken was a 5-hour reset's",
            read_ok.clone(),
            with_go(&|g| g.kind = ResetKind::Session),
            None,
            KeepWhy::Kind,
        ),
        (
            "the confirmation is a full reset's",
            with_read(&|c| c.kind = ResetKind::Full),
            go(),
            None,
            KeepWhy::Kind,
        ),
        (
            "the wall is the session's, the body refills the week",
            read_ok.clone(),
            with_go(&|g| g.limits = vec!["session".to_string()]),
            None,
            KeepWhy::OtherLimit,
        ),
        (
            "the wall's name in another case",
            read_ok.clone(),
            with_go(&|g| g.limits = vec!["Weekly".to_string()]),
            None,
            KeepWhy::OtherLimit,
        ),
        (
            "a wall drawn by no name",
            read_ok.clone(),
            with_go(&|g| g.limits = Vec::new()),
            None,
            KeepWhy::OtherLimit,
        ),
        (
            "the row counted one",
            read_ok.clone(),
            with_go(&|g| g.left = 1),
            None,
            KeepWhy::Count,
        ),
        (
            "the confirmation counts three",
            with_read(&|c| c.left = 3),
            go(),
            None,
            KeepWhy::Count,
        ),
        (
            "the row's day is the day after",
            read_ok.clone(),
            with_go(&|g| g.day = "Oct 6".to_string()),
            None,
            KeepWhy::Day,
        ),
        (
            "the deadline's day only opens with the row's",
            with_read(&|c| c.deadline = "Oct 15 at 3pm".to_string()),
            with_go(&|g| g.day = "Oct 1".to_string()),
            None,
            KeepWhy::Day,
        ),
        (
            "the deadline is in another year",
            with_read(&|c| c.deadline = "Oct 5, 2027 at 3pm".to_string()),
            go(),
            None,
            KeepWhy::Day,
        ),
        (
            "the row's day did not read",
            read_ok.clone(),
            with_go(&|g| g.day = String::new()),
            None,
            KeepWhy::Day,
        ),
        (
            "a short wall, the deadline not placed",
            read_ok.clone(),
            with_go(&|g| g.long = false),
            None,
            KeepWhy::Deadline,
        ),
        (
            "a short wall, the deadline two days off",
            read_ok.clone(),
            with_go(&|g| g.long = false),
            Some(NOW + 48 * HOUR),
            KeepWhy::NotQualifying,
        ),
        (
            "the cursor on the no (the reader's composed screen)",
            read(composed::CONFIRM_FOCUS_NO),
            go(),
            None,
            KeepWhy::Focus,
        ),
    ];
    for (what, read, go, deadline, why) in &rows {
        assert_eq!(
            confirm_verdict(read, go, *deadline, NOW),
            Verdict::Keep(*why),
            "{what}"
        );
    }
    let none_left = with_read(&|c| c.left = 0);
    assert_eq!(
        confirm_verdict(&none_left, &with_go(&|g| g.left = 0), None, NOW),
        Verdict::Keep(KeepWhy::Count)
    );
    let other_and_unfocused = with_read(&|c| {
        c.limits = vec!["session".to_string()];
        c.yes_focused = false;
    });
    assert_eq!(
        confirm_verdict(&other_and_unfocused, &go(), None, NOW),
        Verdict::Keep(KeepWhy::OtherLimit)
    );
    let short_and_unfocused = with_read(&|c| c.yes_focused = false);
    assert_eq!(
        confirm_verdict(
            &short_and_unfocused,
            &with_go(&|g| g.long = false),
            None,
            NOW
        ),
        Verdict::Keep(KeepWhy::Deadline)
    );
}

/// At a wall that is not long, a deadline within a day (to the second) is
/// entered; one a minute past it, one not placed, one now and one in the
/// past are kept.
#[test]
fn a_short_wall_is_reset_only_where_the_offer_expires_within_a_day() {
    let read = read(composed::CONFIRM);
    let short = ResetGo {
        long: false,
        ..go()
    };
    let keep = |why| Verdict::Keep(why);
    for (what, deadline, verdict) in [
        ("23 h 59 m", Some(NOW + 24 * HOUR - 60), Verdict::Enter),
        ("a day to the second", Some(NOW + 24 * HOUR), Verdict::Enter),
        ("a second ahead", Some(NOW + 1), Verdict::Enter),
        (
            "24 h 1 m",
            Some(NOW + 24 * HOUR + 60),
            keep(KeepWhy::NotQualifying),
        ),
        (
            "a day and a second",
            Some(NOW + 24 * HOUR + 1),
            keep(KeepWhy::NotQualifying),
        ),
        ("now", Some(NOW), keep(KeepWhy::Deadline)),
        ("an hour ago", Some(NOW - HOUR), keep(KeepWhy::Deadline)),
        ("the far past", Some(i64::MIN), keep(KeepWhy::Deadline)),
        ("not placed", None, keep(KeepWhy::Deadline)),
    ] {
        assert_eq!(
            confirm_verdict(&read, &short, deadline, NOW),
            verdict,
            "{what}"
        );
        assert_eq!(
            confirm_verdict(&read, &go(), deadline, NOW),
            Verdict::Enter,
            "{what}, at a long wall"
        );
    }
}

/// The trait's defaults are the refusal and `Unread`, so a host that keeps
/// no accounts never has the grant row pressed.
#[test]
fn a_host_that_answers_nothing_takes_no_reset() {
    use crate::supervise::{HostStep, IdleHost};
    #[derive(Debug)]
    struct Silent;
    impl IdleHost for Silent {
        fn wants(&self) -> bool {
            false
        }
        fn at_idle(&self) -> Option<HostStep> {
            None
        }
        fn owns_turn_end(&self) -> bool {
            false
        }
    }
    let ask = ResetAsk {
        kind: ResetKind::Weekly,
        left: 2,
        day: "Oct 5".to_string(),
        day_start: Some(NOW),
    };
    let ends = [true, false].map(|entered| ResetEnd {
        entered,
        kept: (!entered).then_some(KeepWhy::Focus),
        offer: Some(ResetOffer {
            kind: ResetKind::Weekly,
            left: 2,
            use_by: Some(NOW + HOUR),
        }),
        last: !entered,
    });
    assert_eq!(Silent.reset_take(&ask), ResetWord::No(ResetNo::NoHost));
    for end in &ends {
        assert_eq!(Silent.reset_told(end), ResetOutcome::Unread);
    }
}

/// EVERY KEPT RESET'S WORD HAS A PERSON'S WORDS (Settings ▸ Harness's
/// DECIDED FOR YOU): each word the ledger's `declined` row can carry —
/// read through [`fmt::Display`], so a word reworded there is reworded
/// here — reads as a sentence, never the hyphenated code, an inexact
/// read with its part named.
#[test]
fn every_kept_resets_word_has_a_persons_words() {
    for why in KeepWhy::WORDED
        .into_iter()
        .chain([KeepWhy::Inexact("title"), KeepWhy::Inexact("deadline")])
    {
        let word = why.to_string();
        let words = KeepWhy::plain_words(&word).unwrap_or_else(|| panic!("{word}"));
        assert!(
            words.split(' ').count() >= 4 && !words.contains('-'),
            "{word}: {words}"
        );
    }
    assert!(
        KeepWhy::plain_words("inexact-title").is_some_and(|w| w.ends_with("(its title)")),
        "the inexact part is named"
    );
    for unknown in ["frobbed", "inexact-", ""] {
        assert_eq!(KeepWhy::plain_words(unknown), None, "CONTROL: {unknown:?}");
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SETTINGS ▸ HARNESS'S PLAIN COUNTS (design §14.6, slice 6a): WHAT THE HARNESS DID says
//! `past “done”`, `Waited for you`, `Needs, carried on` and `None in 7 days`, and a row's day
//! line reads the same words.

use super::*;
use aterm_agent::harness::stats::Tally;

/// A reading of `today` and `week`.
fn read(today: &HarnessStats, week: &HarnessStats) -> HarnessState {
    let mut state = HarnessState::unobserved();
    state.observed = true;
    state.stats.report = Some(std::sync::Arc::new(HarnessStatsReport::new(
        today.clone(),
        week.clone(),
        1_000_000_000,
        Some(0),
        &|_| Some(0),
    )));
    state
}

/// WHAT THE HARNESS DID's rows of a reading of `today` and `week`.
fn did(today: &HarnessStats, week: &HarnessStats) -> Vec<(String, String)> {
    read(today, week).projection().did_rows
}

/// The words of `label`'s row, if it stands.
fn words_of(rows: &[(String, String)], label: &str) -> Option<String> {
    rows.iter()
        .find(|(l, _)| l == label)
        .map(|(_, w)| w.clone())
}

/// Where `label`'s row stands, if it does.
fn at(rows: &[(String, String)], label: &str) -> Option<usize> {
    rows.iter().position(|(l, _)| l == label)
}

/// The counts `None in 7 days` names, in its order; none where the row does not stand.
pub(super) fn none_named(rows: &[(String, String)]) -> Vec<String> {
    words_of(rows, NONE_IN_7_DAYS)
        .map(|words| {
            words
                .split(crate::native_ui::FACT_SEPARATOR)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Every count WHAT THE HARNESS DID says, each its own row or named in `None in 7 days`, in the
/// order the card stood in before §14.6 (main's `did_rows` at 30b7d2149f): §14.6 keeps it.
const COUNTS: [CountWords; 11] = [
    APPROVED,
    DECLINED,
    ANSWERED,
    KEPT_GOING,
    LANDED,
    LAND_PROMPTS,
    RESUMED,
    RELAUNCHED,
    UPGRADED,
    NEEDED_YOU,
    NEEDS_TOLD,
];

/// `stats` with every count of [`COUNTS`] at `n` (and as long a wait on the person).
fn every_count(n: u64) -> HarnessStats {
    let mut s = HarnessStats::new(0);
    s.approved = n;
    s.declined = n;
    s.answered = n;
    s.continued = n;
    s.landed = n;
    s.land_prompts = n;
    s.resumed = n;
    s.relaunched = n;
    s.upgraded = n;
    s.escalated = n;
    s.needs_told = n;
    s.human_wait_ms = n * 60_000;
    s.rows = 12;
    s
}

/// THE DONE-CHECK ROW READS `past “done”` (§14.6): right under `Kept going`, labelled as a part of
/// it, its value the counts alone (`past “done”  3 today · 3 in 7 days`), only where the week had
/// one; and the unproven row under Approved reads `no safe rule` the same way. The two parts keep
/// two labels, so the page's fact keys stay apart.
#[test]
fn the_done_check_row_reads_past_done() {
    use aterm_agent::supervise::policy::turn_end::{RULE_CONTINUE, RULE_DONE_CHECK};
    let mut today = HarnessStats::new(0);
    today.continued = 5;
    today.approved = 31;
    today.unproven = 2;
    today.by_rule.insert(RULE_CONTINUE.to_string(), 2);
    today.by_rule.insert(RULE_DONE_CHECK.to_string(), 3);
    today.rows = 36;
    let mut week = today.clone();
    week.approved = 212;
    week.unproven = 5;
    week.rows = 217;
    let rows = did(&today, &week);
    assert_eq!(
        words_of(&rows, DONE_CHECKED_OF_KEPT_GOING).as_deref(),
        Some("3 today \u{b7} 3 in 7 days"),
        "{rows:?}"
    );
    assert_eq!(
        at(&rows, DONE_CHECKED_OF_KEPT_GOING),
        at(&rows, KEPT_GOING.label).map(|i| i + 1),
        "right under Kept going: {rows:?}"
    );
    assert_eq!(
        words_of(&rows, UNPROVEN_OF_APPROVED).as_deref(),
        Some("2 today \u{b7} 5 in 7 days"),
        "{rows:?}"
    );
    assert_eq!(
        at(&rows, UNPROVEN_OF_APPROVED),
        at(&rows, APPROVED.label).map(|i| i + 1),
        "right under Approved: {rows:?}"
    );
    let fragment = |label: &str| {
        label
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
    };
    let labels: Vec<String> = rows.iter().map(|(l, _)| fragment(l)).collect();
    let apart: std::collections::BTreeSet<&String> = labels.iter().collect();
    assert_eq!(apart.len(), labels.len(), "one key a row: {rows:?}");
    let mut plain = week.clone();
    plain.by_rule.remove(RULE_DONE_CHECK);
    let mut quiet = today.clone();
    quiet.by_rule.remove(RULE_DONE_CHECK);
    let rows = did(&quiet, &plain);
    assert_eq!(at(&rows, DONE_CHECKED_OF_KEPT_GOING), None, "{rows:?}");
    assert!(
        !none_named(&rows)
            .iter()
            .any(|n| n == DONE_CHECKED_OF_KEPT_GOING),
        "a part is never named: {rows:?}"
    );
}

/// WAITED FOR YOU AND NEEDS, CARRIED ON TELL APART (§14.6): the times the harness stopped and asked,
/// with how long it waited, are one row (`Waited for you  2 today, 14m · 5 in 7 days, 1h 2m`), and a
/// worker's NEEDS YOU the work carried on past is another; a week with no escalation but two needs
/// told gives no `Waited for you` row (it is named in `None in 7 days`) and `Needs, carried on  2
/// today · 2 in 7 days`. No row says `Waited on you`.
#[test]
fn waited_and_carried_on_tell_apart() {
    let mut today = HarnessStats::new(0);
    today.approved = 3;
    today.needs_told = 2;
    today.rows = 5;
    let week = today.clone();
    let rows = did(&today, &week);
    assert_eq!(at(&rows, NEEDED_YOU.label), None, "{rows:?}");
    assert!(
        none_named(&rows).contains(&NEEDED_YOU.many.to_string()),
        "{rows:?}"
    );
    assert_eq!(
        words_of(&rows, NEEDS_TOLD.label).as_deref(),
        Some("2 today \u{b7} 2 in 7 days"),
        "{rows:?}"
    );
    assert!(
        rows.iter().all(|(l, _)| !l.starts_with("Waited on")),
        "{rows:?}"
    );
    let (mut asked_today, mut asked_week) = (today.clone(), week.clone());
    asked_today.escalated = 2;
    asked_today.human_wait_ms = 14 * 60_000;
    asked_week.escalated = 5;
    asked_week.human_wait_ms = 62 * 60_000;
    let rows = did(&asked_today, &asked_week);
    assert_eq!(
        words_of(&rows, NEEDED_YOU.label).as_deref(),
        Some("2 today, 14m \u{b7} 5 in 7 days, 1h 2m"),
        "{rows:?}"
    );
    assert_eq!(
        at(&rows, NEEDS_TOLD.label),
        at(&rows, NEEDED_YOU.label).map(|i| i + 1),
        "{rows:?}"
    );
    assert!(
        !none_named(&rows).contains(&NEEDED_YOU.many.to_string()),
        "{rows:?}"
    );
    asked_today.escalated = 0;
    asked_today.human_wait_ms = 0;
    asked_week.human_wait_ms = 0;
    assert_eq!(
        words_of(&did(&asked_today, &asked_week), NEEDED_YOU.label).as_deref(),
        Some("0 today \u{b7} 5 in 7 days"),
        "a day with none, a week with five asks still open"
    );
    asked_week.escalated = 0;
    asked_week.human_wait_ms = 3 * 60_000;
    let rows = did(&asked_today, &asked_week);
    assert_eq!(
        words_of(&rows, NEEDED_YOU.label).as_deref(),
        Some("0 today \u{b7} 0 in 7 days, 3m"),
        "a wait in the week on an older ask is a wait for you: {rows:?}"
    );
    assert!(
        !none_named(&rows).contains(&NEEDED_YOU.many.to_string()),
        "{rows:?}"
    );
}

/// A WEEK OF NOTHING IS ONE ROW (§14.6): every count whose week is 0 is named, in its words, in
/// one row, `None in 7 days`, in the order the counts stand as rows (Waited for you and Needs,
/// carried on included), as the last of the counts (over `Waited at limits`); a count with a week
/// is its own row and is not named there; no row reads `0 today · 0 in 7 days`.
#[test]
fn a_week_of_nothing_is_one_row() {
    let rows = did(&every_count(1), &every_count(1));
    assert_eq!(at(&rows, NONE_IN_7_DAYS), None, "{rows:?}");
    let order: Vec<&str> = rows
        .iter()
        .filter_map(|(l, _)| COUNTS.iter().find(|c| c.label == l))
        .map(|c| c.many)
        .collect();
    assert_eq!(
        order,
        COUNTS.iter().map(|c| c.many).collect::<Vec<_>>(),
        "every count is a row, in the card's order: {rows:?}"
    );
    let mut nothing = HarnessStats::new(0);
    nothing.rows = 1;
    let rows = did(&nothing, &nothing);
    assert_eq!(none_named(&rows), order, "in the card's order: {rows:?}");
    assert!(
        rows.iter()
            .all(|(l, _)| COUNTS.iter().all(|c| c.label != l)),
        "no count is a row of its own: {rows:?}"
    );
    assert!(
        rows.iter().all(|(_, w)| !w.ends_with(" 0 in 7 days")),
        "{rows:?}"
    );
    assert_eq!(
        at(&rows, NONE_IN_7_DAYS).map(|i| i + 1),
        at(&rows, "Waited at limits"),
        "the last of the counts: {rows:?}"
    );
    let mut some = nothing.clone();
    some.approved = 31;
    some.land_prompts = 2;
    some.needs_told = 2;
    let rows = did(&some, &some);
    let named = none_named(&rows);
    for gone in [APPROVED, LAND_PROMPTS, NEEDS_TOLD] {
        assert!(at(&rows, gone.label).is_some(), "{gone:?}: {rows:?}");
        assert!(
            !named.contains(&gone.many.to_string()),
            "{gone:?}: {rows:?}"
        );
    }
    let rest: Vec<&str> = order
        .iter()
        .copied()
        .filter(|w| ![APPROVED.many, LAND_PROMPTS.many, NEEDS_TOLD.many].contains(w))
        .collect();
    assert_eq!(named, rest, "{rows:?}");
    assert!(
        named.contains(&NEEDED_YOU.many.to_string()),
        "waited for you, its week 0: {rows:?}"
    );
    let empty = HarnessStats::new(0);
    let rows = did(&empty, &empty);
    assert_eq!(
        at(&rows, NONE_IN_7_DAYS),
        None,
        "nothing yet says it: {rows:?}"
    );
}

/// A ROW'S DAY LINE READS THE PAGE'S WORDS (§14.6): the ask with its wait and clock in
/// [`NEEDED_YOU`]'s words, then the needs in [`NEEDS_TOLD`]'s, then the families (`today: waited
/// for you once, 1m at 18:29 · 2 needs, carried on · 1 landed · 31 approved`); a day whose wait
/// stands on an ask raised before it says that wait in the same words (`today: waited for you 3m ·
/// 1 approved`), as the page's Waited for you row and STATUS's today line (`waited for you 4m`) do.
#[test]
fn the_day_line_reads_waited_for_you_and_carried_on() {
    let line =
        |parts: &[String]| format!("today: {}", parts.join(crate::native_ui::FACT_SEPARATOR));
    let waited = |ms: u64| format!("{} {}", NEEDED_YOU.one, duration_words(ms));
    let at_18_29 = (18 * 3_600 + 29 * 60) * 1000;
    let tally = Tally {
        escalated: 1,
        human_wait_ms: 60_000,
        last_escalated_ms: Some(at_18_29),
        needs_told: 2,
        landed: 1,
        approved: 31,
        ..Tally::default()
    };
    assert_eq!(
        day_line(&tally, Some(0)),
        Some(line(&[
            format!(
                "{} {}, {} at {}",
                NEEDED_YOU.one,
                times_words(1),
                duration_words(60_000),
                clock_words(at_18_29, Some(0))
            ),
            NEEDS_TOLD.count(2),
            LANDED.count(1),
            APPROVED.count(31),
        ]))
    );
    let one = Tally {
        needs_told: 1,
        ..Tally::default()
    };
    assert_eq!(day_line(&one, Some(0)), Some(line(&[NEEDS_TOLD.count(1)])));
    let older_ask = Tally {
        human_wait_ms: 3 * 60_000,
        approved: 1,
        ..Tally::default()
    };
    assert_eq!(
        day_line(&older_ask, Some(0)),
        Some(line(&[waited(3 * 60_000), APPROVED.count(1)])),
        "a wait today on an ask raised before the day"
    );
    let mut today = HarnessStats::new(0);
    today.approved = 1;
    today.human_wait_ms = 4 * 60_000;
    today.rows = 1;
    let (_, rest) = read(&today, &today).today_lines().expect("a day");
    assert_eq!(rest, waited(4 * 60_000));
}

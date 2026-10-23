// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests verifying that occupancy fact rows (`fact-hold@v1` `typed`, and a held verdict's
//! `fact-held@v1`) do not affect act readers.

use super::*;
use crate::supervise::facts::FactKind;
use crate::supervise::policy::turn_end::{
    AccountAt, RULE_ACCOUNT, RULE_CONTINUE, RULE_DONE_CHECK, RULE_LAND, RULE_LEFT,
    RULE_LIMIT_RESUME, RULE_LOGIN, RULE_MODEL_FALLBACK,
};

const SID: &str = "@s-1";

fn row_at(ts: i64, rule_id: &str, outcome: Outcome, command: &str, reason: &str) -> String {
    Row {
        rule_id,
        outcome,
        command,
        reason,
        box_seq: 1,
    }
    .to_json(ts, Some(SID))
}

fn fact_typed(ts: i64) -> String {
    let line = crate::supervise::occupancy::templates::wait_nudge(30 * 60_000);
    row_at(
        ts,
        FactKind::Hold.rule(),
        Outcome::Typed,
        line.text(),
        "rule=wait-nudge@v1 episode=00000000000000aa held_ms=1800000",
    )
}

/// A held verdict's row (`fact-held@v1`, design §6): `waiting`, its episode and guard.
fn fact_held(ts: i64) -> String {
    fact_held_of(ts, SID)
}

/// [`fact_held`], booked for session `sid`.
fn fact_held_of(ts: i64, sid: &str) -> String {
    use crate::supervise::occupancy::rules::{HELD_RULE, RuleId, held_reason};
    Row {
        rule_id: HELD_RULE,
        outcome: Outcome::Waiting,
        command: "",
        reason: &held_reason(
            RuleId::WaitNudge,
            0xaa,
            0,
            1_800_000,
            false,
            crate::supervise::Guard::Person,
        ),
        box_seq: 1,
    }
    .to_json(ts, Some(sid))
}

fn body(rows: &[String]) -> String {
    rows.iter().map(|r| format!("{r}\n")).collect()
}

fn invisible<T: PartialEq + std::fmt::Debug>(before: &[String], read: impl Fn(&str) -> T) {
    invisible_at(before, before.len(), read);
}

/// The reader `read` of the ledger `rows` with a fact row put at `at`: the door's typed line and
/// a held verdict's row each move it nothing. A held row is `waiting` under a rule no reader
/// names, so even read as an act it moves nothing. NEGATIVE CONTROL: read as an act, the typed
/// line at the same place moves it, so the reader sees what stands there.
fn invisible_at<T: PartialEq + std::fmt::Debug>(
    rows: &[String],
    at: usize,
    read: impl Fn(&str) -> T,
) {
    let plain = body(rows);
    let with = |fact: String| {
        let mut rows = rows.to_vec();
        rows.insert(at, fact);
        body(&rows)
    };
    let told = with(fact_typed(9_000));
    let held = with(fact_held(9_000));
    assert_eq!(
        read(&told),
        read(&plain),
        "a fact line moves no reader of acts"
    );
    assert_eq!(
        read(&held),
        read(&plain),
        "a held verdict's row moves no reader of acts"
    );
    assert_eq!(
        with_facts_counted(|| read(&held)),
        with_facts_counted(|| read(&plain)),
        "read as an act, a held verdict's row is still no act: it is waiting"
    );
    assert_ne!(
        with_facts_counted(|| read(&told)),
        with_facts_counted(|| read(&plain)),
        "read as an act, the fact line moves it"
    );
}

fn continued(ts: i64) -> String {
    row_at(ts, RULE_CONTINUE, Outcome::Typed, "keep going", "-")
}

#[test]
fn a_fact_line_breaks_no_short_streak() {
    let act = row_at(
        1_000,
        RULE_CONTINUE,
        Outcome::Typed,
        "keep going",
        &act_reason_after(Carried::streak(2), None),
    );
    let point = row_at(
        2_000,
        RULE_POINT,
        Outcome::Waiting,
        "",
        &point_reason(Carried::streak(3), false),
    );
    invisible(&[act, point], |b| {
        short_streak_in(b, Some(SID)).map(|s| (s.at_ms, s.short, s.point))
    });
}

#[test]
fn a_fact_line_is_no_act_typed_at() {
    invisible(&[continued(1_000)], |b| typed_at_in(b, Some(SID)));
}

#[test]
fn a_fact_line_is_not_the_last_act() {
    invisible(&[continued(1_000)], |b| {
        last_act_count_in(b, Some(SID)).map(|(at, _)| at)
    });
}

#[test]
fn a_fact_line_is_no_text_the_loop_typed() {
    let dir = crate::supervise::ScratchDir::new("apr-fact", "texts");
    let path = dir.join("s-1.jsonl");
    invisible(&[continued(1_000)], |b| {
        std::fs::write(&path, b).expect("write");
        typed_texts(&path, Some(SID))
    });
}

#[test]
fn a_fact_line_keeps_the_login_typed_last() {
    let login = row_at(1_000, RULE_LOGIN, Outcome::Typed, "/login", "-");
    invisible(&[login], |b| login_typed_last_in(b, Some(SID)));
}

#[test]
fn a_fact_line_ends_no_persons_exchange() {
    let left = row_at(
        1_000,
        RULE_LEFT,
        Outcome::Skipped,
        &left_before_key(1_000),
        LEFT_REASON,
    );
    invisible(&[left], |b| open_left_in(b, Some(SID)));
}

#[test]
fn a_fact_line_keeps_the_land_episode() {
    let land = row_at(
        1_000,
        RULE_LAND,
        Outcome::Typed,
        "land the work",
        &act_reason(None),
    );
    invisible(&[land], |b| {
        land_episode_in(b, Some(SID)).map(|e| (e.landing, e.nudges, e.owed, e.act.is_some()))
    });
}

#[test]
fn a_fact_line_is_no_try_awaited() {
    let resume = row_at(1_000, RULE_LIMIT_RESUME, Outcome::Typed, "go on", "-");
    invisible(&[resume], |b| awaited_try_in(b, Some(SID)).map(|a| a.at_ms));
}

#[test]
fn a_fact_line_keeps_a_carry_ended() {
    let esc = row_at(
        2_000,
        RULE_POINT,
        Outcome::Waiting,
        "",
        &point_reason(Carried::streak(1), true),
    );
    invisible(&[continued(1_000), esc], |b| carry_ended_in(b, Some(SID)));
}

#[test]
fn a_fact_line_keeps_a_relaunch_waiting() {
    let waits = row_at(
        1_000,
        RULE_MODEL_FALLBACK,
        Outcome::Waiting,
        "",
        &relaunch_waits_reason(5),
    );
    invisible(&[waits], |b| relaunch_waiting_in(b, Some(SID)));
}

#[test]
fn a_fact_line_keeps_a_done_check_owed() {
    let check = row_at(
        1_000,
        RULE_DONE_CHECK,
        Outcome::Typed,
        super::super::policy::turn_end::DONE_CHECK,
        "-",
    );
    invisible(&[check], |b| {
        done_check_owed_in(b, Some(SID)).map(|c| c.rule)
    });
}

#[test]
fn a_fact_line_keeps_a_wall_open() {
    let wall = row_at(
        1_000,
        RULE_WALL_WAIT,
        Outcome::Waiting,
        "",
        &wall_wait_reason(
            Some(2_000),
            Some("3pm"),
            aterm_phase::WallKind::UsageSession,
            "Session limit reached",
        ),
    );
    invisible(&[wall], |b| open_wall_in(b, Some(SID)).is_some());
}

#[test]
fn a_fact_line_in_the_transcript_is_no_task() {
    let line = crate::supervise::occupancy::templates::wait_nudge(30 * 60_000);
    let user = |text: &str| {
        let content = json_str(text);
        format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":{content}}},\
             \"origin\":{{\"kind\":\"human\"}},\"isSidechain\":false}}\n"
        )
    };
    assert!(!crate::harness::upgrade::transcript_tasked(
        &user(line.text()),
        &[]
    ));
    let unmarked = line
        .text()
        .strip_prefix(crate::harness::upgrade::HARNESS_MARK)
        .expect("the line opens with the mark");
    assert!(crate::harness::upgrade::transcript_tasked(
        &user(unmarked),
        &[]
    ));
}

#[test]
fn a_fact_line_is_no_read_after_a_usage_try() {
    let resume = row_at(1_000, RULE_LIMIT_RESUME, Outcome::Typed, "go on", "-");
    let answered = row_at(10_000, RULE_TRY_ANSWERED, Outcome::Waiting, "", "-");
    invisible_at(&[resume, answered], 1, |b| {
        usage_attempt_taken_in(b, Some(SID), &|_, _| false).map(|a| a.read_ms)
    });
}

#[test]
fn a_fact_line_asks_nothing_on_a_model_switch() {
    let switch = row_at(
        1_000,
        RULE_MODEL_FALLBACK,
        Outcome::Typed,
        "relaunch --model sonnet",
        &model_switch_reason(Some("opus"), None, 1),
    );
    invisible(&[switch], |b| {
        open_model_switch_in(b, Some(SID)).map(|o| o.asked)
    });
}

#[test]
fn a_fact_line_asks_nothing_after_an_account_move() {
    let moved = row_at(
        1_000,
        RULE_ACCOUNT,
        Outcome::Typed,
        &format!("{ACCOUNT_MOVE_COMMAND}work"),
        &account_switch_reason("work", AccountAt::Wall),
    );
    invisible(&[moved], |b| {
        account_moves_in(b, Some(SID)).open.map(|o| o.asked)
    });
}

/// A HELD VERDICT'S ROW IS A FACT ROW (design §6): its rule opens with the facts seam's prefix,
/// so every reader of acts walks past it ([`act_lines`]), and it reads back as the episode it
/// books (its rule and its ordinal in the hold), the guard and its stamp, for its own session
/// alone: `@s-1` reads no row of `s-10`, whose sid holds its own. CONTROL: a row of a policy
/// rule is no fact row, and `@s-10` reads its own row.
#[test]
fn a_held_row_is_a_fact_row_and_reads_back_its_episode_and_guard() {
    use crate::supervise::occupancy::rules::{HELD_RULE, HeldRow, RuleId, held_rows_in};
    let held = fact_held(1_000);
    assert!(is_fact_rule(HELD_RULE));
    assert!(is_fact_line(&held), "{held}");
    assert!(!is_fact_line(&continued(1_000)));
    assert_eq!(act_lines(&body(std::slice::from_ref(&held))).count(), 0);
    let row = HeldRow {
        rule: RuleId::WaitNudge,
        ordinal: 0,
        guard: crate::supervise::Guard::Person,
        row_ms: 1_000,
    };
    assert_eq!(
        held_rows_in(&body(&[held, continued(2_000)]), Some(SID)),
        [row]
    );
    assert!(held_rows_in(&body(&[fact_held(1_000)]), Some("@s-2")).is_empty());
    let other = body(&[fact_held_of(1_000, "@s-10")]);
    assert!(held_rows_in(&other, Some(SID)).is_empty(), "{other}");
    assert_eq!(held_rows_in(&other, Some("@s-10")), [row]);
}

#[test]
fn no_command_or_reason_forges_a_fact_row() {
    let spelled = format!(",\"rule_id\":\"{}\",", FactKind::Hold.rule());
    let forged = row_at(1_000, RULE_CONTINUE, Outcome::Typed, &spelled, &spelled);
    assert!(forged.contains(FACT_RULE_PREFIX), "{forged}");
    assert!(!is_fact_line(&forged), "{forged}");
    assert_eq!(typed_at_in(&body(&[forged]), Some(SID)), [1_000]);
}

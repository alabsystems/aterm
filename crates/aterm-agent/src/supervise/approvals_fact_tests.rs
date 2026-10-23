// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests verifying that occupancy fact rows (`fact-hold@v1`) do not affect act readers.

use super::*;
use crate::supervise::facts::FactKind;
use crate::supervise::policy::turn_end::{
    AccountAt, RULE_ACCOUNT, RULE_CONTINUE, RULE_DONE_CHECK, RULE_LAND, RULE_LEFT,
    RULE_LIMIT_RESUME, RULE_LOGIN, RULE_MODEL_FALLBACK,
};

/// The session every row is of.
const SID: &str = "@s-1";

/// One of the session's rows at `ts`.
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

/// The occupancy door's line at `ts`, booked as the door books it.
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

/// `rows` as a ledger's text.
fn body(rows: &[String]) -> String {
    rows.iter().map(|r| format!("{r}\n")).collect()
}

/// `read` reads the rows `before` as it reads them with a fact line typed after them; read as an
/// act, the fact line moves it.
fn invisible<T: PartialEq + std::fmt::Debug>(before: &[String], read: impl Fn(&str) -> T) {
    invisible_at(before, before.len(), read);
}

/// [`invisible`] with the fact line typed among `rows`, before the one at `at`.
fn invisible_at<T: PartialEq + std::fmt::Debug>(
    rows: &[String],
    at: usize,
    read: impl Fn(&str) -> T,
) {
    let plain = body(rows);
    let mut rows = rows.to_vec();
    rows.insert(at, fact_typed(9_000));
    let told = body(&rows);
    assert_eq!(
        read(&told),
        read(&plain),
        "a fact line moves no reader of acts"
    );
    assert_ne!(
        with_facts_counted(|| read(&told)),
        with_facts_counted(|| read(&plain)),
        "read as an act, the fact line moves it"
    );
}

/// A continuation typed at `ts`.
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

/// The line itself opens with the harness's mark, so the conversation's task scan reads it as
/// the harness's, never a person's task ([`crate::harness::upgrade::transcript_tasked`]); the
/// same words without the mark task the conversation.
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

/// A usage limit's try typed, a fact line, then the try's answer read: the answer is still the
/// first read after the try.
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

/// NO ROW FORGES A FACT ROW ([`is_fact_line`]): a continuation whose command and reason spell a
/// fact rule's field is an act, read as one. CONTROL: its line holds the fact rule's name, so a
/// reader that matched the name alone would drop it.
#[test]
fn no_command_or_reason_forges_a_fact_row() {
    let spelled = format!(",\"rule_id\":\"{}\",", FactKind::Hold.rule());
    let forged = row_at(1_000, RULE_CONTINUE, Outcome::Typed, &spelled, &spelled);
    assert!(forged.contains(FACT_RULE_PREFIX), "{forged}");
    assert!(!is_fact_line(&forged), "{forged}");
    assert_eq!(typed_at_in(&body(&[forged]), Some(SID)), [1_000]);
}

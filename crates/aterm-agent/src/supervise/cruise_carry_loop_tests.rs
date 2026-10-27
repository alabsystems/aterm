// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for Cruise's carry at a break of the agent's own background work
//! (`Session::cruise_carry`) and the needs such a break's report names (`Session::break_needs`):
//! CRUISE NEVER PARKS (owner ruling 2026-10-26).

use super::*;
use crate::supervise::policy::cruise_carry::{BACKGROUND_CARRY, RULE_BACKGROUND};

/// A turn that ended with the agent's own dynamic workflows running, its report `said` (each
/// line under the first in the reply's gutter).
fn waiting(said: &[&str]) -> Vec<String> {
    let mut lines: Vec<String> = said
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                format!("⏺ {l}")
            } else {
                format!("  {l}")
            }
        })
        .collect();
    lines.extend(
        ["", "✻ Waiting for 2 dynamic workflows to finish", ""]
            .iter()
            .map(ToString::to_string),
    );
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut r = rows(&refs);
    r.extend(composer("  ⏵⏵ auto mode on · 2 workflows · ← for agents"));
    r
}

const PLAIN: &[&str] = &["Both workflows are running; I will pick up when they finish."];
const NEEDS: &[&str] = &[
    "Both workflows are running; the docs are next.",
    "NEEDS YOU: the release credential to sign 0.145.0",
];
const DONE: &[&str] = &[
    "Nothing left: the fix is on main and every check passed.",
    "ATERM-TASK-DONE",
];

/// The policy of the run: `autopilot`, the occupancy door at its default (`observe`, which types
/// nothing of its own).
fn under(autopilot: Autopilot) -> SupervisorConfig {
    SupervisorConfig {
        autopilot,
        continue_policy: true,
        ..policy(Occupancy::Observe)
    }
}

/// The breaks of one turn that ended on its background work, read at `at_s`, under `autopilot`,
/// the report `said`, the mock as `set` leaves it.
fn ran(
    tag: &str,
    autopilot: Autopilot,
    said: &'static [&'static str],
    at_s: &[i64],
    set: &dyn Fn(&mut Mock),
    before: Before<'_>,
) -> Ran {
    let drawn = move |_| waiting(said);
    Script {
        policy: under(autopilot),
        drawn: &drawn,
        set,
        ..Script::new(tag, Occupancy::Observe, at_s)
    }
    .run(before)
}

/// The carries the run typed: each `send` of [`BACKGROUND_CARRY`].
fn carries(ran: &Ran) -> usize {
    ran.writes()
        .iter()
        .filter(|w| w.starts_with("send ") && w.ends_with(&format!(" -- {BACKGROUND_CARRY}")))
        .count()
}

/// The `meta set attention` requests the run made: the needs entry raised.
fn raised(ran: &Ran) -> Vec<String> {
    ran.requests()
        .into_iter()
        .filter(|r| r.starts_with("meta set attention "))
        .collect()
}

#[test]
fn cruise_carries_a_settled_background_break_and_off_and_land_type_nothing() {
    let cruise = ran(
        "carry-cruise",
        Autopilot::Cruise,
        PLAIN,
        &[0, 30],
        &|_| {},
        &mut nothing,
    );
    assert_eq!(carries(&cruise), 1, "{:#?}", cruise.writes());
    let writes = cruise.writes();
    assert!(
        writes[0].starts_with("send if-human=7 if-gen=1."),
        "fenced on the person's last key and the screen: {writes:#?}"
    );
    assert!(
        writes[1].starts_with("key if-human=7 if-gen=1.") && writes[1].ends_with(" enter"),
        "{writes:#?}"
    );
    let requests = cruise.requests();
    let status = requests.iter().position(|r| r == "status").expect("status");
    let send = requests
        .iter()
        .position(|r| r.starts_with("send "))
        .expect("send");
    assert!(
        status < send,
        "the person's hands are read just before the write"
    );
    assert!(
        cruise
            .ledger
            .contains(&format!("\"rule_id\":\"{RULE_BACKGROUND}\"")),
        "{}",
        cruise.ledger
    );
    assert!(
        cruise.calls.iter().any(|c| c == "break_taken"),
        "the host hears the break is taken: {:?}",
        cruise.calls
    );
    for mode in [Autopilot::Off, Autopilot::Land] {
        let other = ran("carry-other", mode, PLAIN, &[0, 30], &|_| {}, &mut nothing);
        assert!(other.writes().is_empty(), "{mode:?}: {:#?}", other.writes());
        assert!(
            !other.requests().iter().any(|r| r == "status"),
            "{mode:?} reads nothing for a write it never makes: {:?}",
            other.requests()
        );
    }
}

#[test]
fn a_person_typing_or_their_draft_holds_the_carry() {
    let typing = ran(
        "carry-person",
        Autopilot::Cruise,
        PLAIN,
        &[0, 30],
        &|m| m.human_ms = Some(1_000),
        &mut nothing,
    );
    assert!(typing.writes().is_empty(), "{:#?}", typing.writes());
    assert!(
        typing.noted.iter().any(|l| l.starts_with("DEFERRED ")
            && l.contains(RULE_BACKGROUND)
            && l.ends_with("held:person")),
        "{:#?}",
        typing.noted
    );
    // A draft in the composer, the cursor after it, is the person's too.
    let drafted_run = Script {
        policy: under(Autopilot::Cruise),
        drawn: &|_| drafted(waiting(PLAIN), "wait, one more thing"),
        set: &|m| m.cursor_col = Some(22),
        ..Script::new("carry-draft", Occupancy::Observe, &[0, 30])
    }
    .run(&mut nothing);
    assert!(
        drafted_run.writes().is_empty(),
        "{:#?}",
        drafted_run.writes()
    );
    // The draft is the person's last key too: either guard holds it.
    assert!(
        drafted_run.noted.iter().any(|l| l.starts_with("DEFERRED ")
            && (l.ends_with("held:draft") || l.ends_with("held:person"))),
        "{:#?}",
        drafted_run.noted
    );
}

#[test]
fn a_needs_you_report_at_a_break_is_raised_and_the_work_is_carried() {
    let cruise = ran(
        "carry-needs",
        Autopilot::Cruise,
        NEEDS,
        &[0, 30],
        &|_| {},
        &mut nothing,
    );
    let up = raised(&cruise);
    assert_eq!(up.len(), 1, "raised once, not at every read: {up:#?}");
    assert!(up[0].contains("the release credential"), "{up:#?}");
    assert_eq!(
        carries(&cruise),
        1,
        "and the work goes on: {:#?}",
        cruise.writes()
    );
    // Off types nothing and raises nothing.
    let off = ran(
        "carry-needs-off",
        Autopilot::Off,
        NEEDS,
        &[0, 30],
        &|_| {},
        &mut nothing,
    );
    assert!(
        off.writes().is_empty() && raised(&off).is_empty(),
        "{:#?}",
        off.requests()
    );
    // A person typing holds the carry, never the needs entry.
    let typing = ran(
        "carry-needs-person",
        Autopilot::Cruise,
        NEEDS,
        &[0, 30],
        &|m| m.human_ms = Some(1_000),
        &mut nothing,
    );
    assert!(typing.writes().is_empty(), "{:#?}", typing.writes());
    assert_eq!(raised(&typing).len(), 1, "{:#?}", typing.requests());
}

#[test]
fn a_done_report_at_a_break_ends_nothing_under_cruise() {
    let cruise = ran(
        "carry-done",
        Autopilot::Cruise,
        DONE,
        &[0, 30],
        &|_| {},
        &mut nothing,
    );
    assert_eq!(carries(&cruise), 1, "{:#?}", cruise.writes());
    let off = ran(
        "carry-done-off",
        Autopilot::Off,
        DONE,
        &[0, 30],
        &|_| {},
        &mut nothing,
    );
    assert!(off.writes().is_empty(), "{:#?}", off.writes());
    let typing = ran(
        "carry-done-person",
        Autopilot::Cruise,
        DONE,
        &[0, 30],
        &|m| m.human_ms = Some(1_000),
        &mut nothing,
    );
    assert!(typing.writes().is_empty(), "{:#?}", typing.writes());
}

/// A carry the agent answers in seconds, its turn ending on the work again, is asked again on the
/// policy's short-turn ladder (2 min, then 4), never every read and never never.
#[test]
fn a_short_answer_backs_the_next_carry_off_on_the_ladder() {
    let cfg = under(Autopilot::Cruise);
    let ran = ran(
        "carry-ladder",
        Autopilot::Cruise,
        PLAIN,
        // The first carry at 30 s; the agent answers it at 40 s and its turn ends on the work
        // again: 60 s is short of the 2-minute back-off, 160 s past it; the next answer comes at
        // 170 s, and 300 s is short of 4 minutes after 160, 410 s past it.
        &[30, 40, 60, 160, 170, 300, 410],
        &|_| {},
        &mut |s, i| {
            if i == 1 || i == 4 {
                s.took(7, Some(&cfg), &mut Heard::default());
            }
        },
    );
    assert_eq!(carries(&ran), 3, "{:#?}", ran.writes());
    assert!(
        ran.noted
            .iter()
            .any(|l| l.starts_with("DEFERRED ") && l.contains("(1 short answer(s) in a row)")),
        "{:#?}",
        ran.noted
    );
}

/// The session's own mode, switched from the chip since the loop last read it, is read afresh
/// just before the carry is typed: Land or Off there types nothing, Cruise there types it.
#[test]
fn a_switch_since_the_loops_last_read_is_read_before_the_carry() {
    for (word, typed) in [("off", 0), ("land", 0), ("cruise", 1)] {
        let meta = format!(
            "OK title=- attention=- cwd=- state=idle questions=- autopilot={word} land_at=-\n"
        );
        let set = move |m: &mut Mock| {
            m.verb_replies
                .entry("meta")
                .or_default()
                .push_back(ok(&meta));
        };
        let run = ran(
            "carry-switched",
            Autopilot::Cruise,
            PLAIN,
            &[0, 30],
            &set,
            &mut nothing,
        );
        let requests = run.requests();
        assert_eq!(carries(&run), typed, "{word}: {requests:#?}");
        let status = requests.iter().position(|r| r == "status");
        let read = requests.iter().rposition(|r| r == "meta");
        assert!(
            status.is_some() && read > status,
            "{word}: the mode is read after the hands, before the write: {requests:#?}"
        );
    }
}

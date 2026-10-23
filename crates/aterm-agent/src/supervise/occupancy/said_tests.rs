// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `OccupancySaid`: its change, the Entry it names under `prompt`, and its `why` form, asserted by structure (each line's head, its closed words and its marks), not its prose.

use super::*;
use crate::supervise::config::Autopilot;
use crate::supervise::occupancy::SlotsSeen;

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;
/// 2026-10-19 18:52:03 UTC.
const SINCE: i64 = 1_792_435_923_000;

fn reading(at_ms: i64) -> OccupancyReading {
    OccupancyReading {
        taken_ms: at_ms,
        stale: false,
        avail_mib: Some(96 << 10),
        pressure: Some(Pressure::Normal),
        disk: Some(Level::Ok),
        slots: Some(SlotsSeen { usable: 1, n: 3 }),
        slots_free_ms: Some(20 * MIN as u64),
        room: Some(true),
        held_slot_last_ms: Some(at_ms - MIN),
    }
}

/// The guards of a Cruise tab with `held` holding, the rest clear.
fn holding(held: &[Guard]) -> Guards {
    let clear = Guards::clear(Autopilot::Cruise);
    Guards {
        person: held.contains(&Guard::Person),
        draft: held.contains(&Guard::Draft),
        memory: held.contains(&Guard::Memory),
        ..clear
    }
}

fn said(mode: Occupancy, verdicts: [SaidVerdict; 2], guards: Option<Guards>) -> OccupancySaid {
    let at_ms = SINCE + 5 * HOUR + 25 * MIN;
    OccupancySaid {
        mode,
        hold: Some(SaidHold {
            since_ms: SINCE,
            declared: false,
        }),
        ready: true,
        guards,
        rules: [
            SaidRule {
                rule: RuleId::WaitNudge,
                verdict: verdicts[0],
                at_ms: SINCE + 30 * MIN,
            },
            SaidRule {
                rule: RuleId::HoldRoom,
                verdict: verdicts[1],
                at_ms,
            },
        ],
        at_ms,
        reading: reading(at_ms),
    }
}

/// The change is the mode, the hold, the ready proxy, the guard and each rule's word; the stamp and the reading alone are not.
#[test]
fn a_new_reading_or_stamp_alone_is_no_change() {
    let base = said(
        Occupancy::Observe,
        [SaidVerdict::Would, SaidVerdict::Quiet],
        None,
    );
    let mut later = base.clone();
    later.at_ms += MIN;
    later.reading.avail_mib = Some(1 << 10);
    assert!(base.same_change(&later));

    let mut held = base.clone();
    held.rules[1].verdict = SaidVerdict::Held(Guard::Person);
    assert!(!base.same_change(&held));
    let mut guarded = base.clone();
    guarded.guards = Some(holding(&[Guard::Draft]));
    assert!(!base.same_change(&guarded));
    let mut both = guarded.clone();
    both.guards = Some(holding(&[Guard::Draft, Guard::Memory]));
    assert_eq!(both.guard(), guarded.guard(), "the first guard is the same");
    assert!(!guarded.same_change(&both));
    let mut closed = base.clone();
    closed.hold = None;
    assert!(!base.same_change(&closed));
    let mut prompt = base.clone();
    prompt.mode = Occupancy::Prompt;
    assert!(!base.same_change(&prompt));
}

/// A spent cap names an Entry only under `prompt`, with a hold open; under `observe` nothing the owner sees was spent (the negative control).
#[test]
fn a_spent_cap_is_named_under_prompt_alone() {
    let spent = [SaidVerdict::Quiet, SaidVerdict::Spent];
    assert_eq!(
        said(Occupancy::Prompt, spent, None).spent_under_prompt(),
        vec![RuleId::HoldRoom]
    );
    assert!(
        said(Occupancy::Observe, spent, None)
            .spent_under_prompt()
            .is_empty()
    );
    let mut closed = said(Occupancy::Prompt, spent, None);
    closed.hold = None;
    assert!(closed.spent_under_prompt().is_empty());
    assert!(
        said(
            Occupancy::Prompt,
            [SaidVerdict::Typed, SaidVerdict::Would],
            None
        )
        .spent_under_prompt()
        .is_empty()
    );
}

/// `occupancy why` against a scripted `OccupancySaid`: the head line names the mode and the hold, a line a rule in `RuleId::ALL`'s order carries its verdict's words and a mark a term, and the guards' line names each guard of §2.1 with its mark, a cross where it holds (§6).
#[test]
fn why_renders_each_rule_and_each_guard() {
    let s = said(
        Occupancy::Observe,
        [SaidVerdict::Would, SaidVerdict::Held(Guard::Person)],
        Some(holding(&[Guard::Person, Guard::Memory])),
    );
    let lines = why_lines(&s, s.at_ms, 0);
    assert_eq!(lines.len(), 4, "{lines:#?}");
    let words = |line: &str| {
        line.split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let head = words(&lines[0]);
    assert_eq!(head[0], "occupancy=observe", "{lines:#?}");
    assert!(head.contains(&"undeclared".to_string()), "{lines:#?}");
    assert!(head.contains(&"ready=1".to_string()), "{lines:#?}");

    let nudge = words(&lines[1]);
    assert_eq!(nudge[0], "wait-nudge@v1", "{lines:#?}");
    assert!(nudge.contains(&"would".to_string()), "{lines:#?}");
    let room = words(&lines[2]);
    assert_eq!(room[0], "hold-room@v1", "{lines:#?}");
    assert!(room.contains(&"held:person".to_string()), "{lines:#?}");
    assert!(room.contains(&"1/3".to_string()), "{lines:#?}");
    assert!(!lines[2].contains('✗'), "{lines:#?}");
    assert!(lines[2].matches('✓').count() >= 6, "{lines:#?}");

    let marks = |line: &str| -> Vec<(String, String)> {
        let w = words(line);
        assert_eq!(w[0], "guards:", "{line}");
        let mut each = Vec::new();
        let mut at = 1;
        while at < w.len() {
            let wide = matches!(w[at].as_str(), "brakes" | "autopilot");
            let mark = if wide { at + 2 } else { at + 1 };
            each.push((w[at].clone(), w[mark].clone()));
            at = mark + 1;
        }
        each
    };
    let guards = marks(&lines[3]);
    let names: Vec<&str> = guards.iter().map(|(g, _)| g.as_str()).collect();
    let mut want = vec!["brakes"];
    want.extend(
        Guards::clear(Autopilot::Cruise)
            .each(RuleId::WaitNudge)
            .iter()
            .map(|(guard, _)| guard.word()),
    );
    assert_eq!(names, want, "{lines:#?}");
    let crossed: Vec<&str> = guards
        .iter()
        .filter(|(_, mark)| mark == "✗")
        .map(|(g, _)| g.as_str())
        .collect();
    assert_eq!(crossed, ["person", "memory"], "{lines:#?}");

    let mut clear = s.clone();
    clear.guards = Some(holding(&[]));
    clear.reading.stale = true;
    clear.reading.disk = Some(Level::Low);
    let lines = why_lines(&clear, clear.at_ms, 0);
    assert!(
        marks(&lines[3]).iter().all(|(_, mark)| mark == "✓"),
        "{lines:#?}"
    );
    assert!(lines[2].contains('✗'), "{lines:#?}");
    clear.guards = None;
    let lines = why_lines(&clear, clear.at_ms, 0);
    assert_eq!(
        words(&lines[3]),
        ["guards:", "none", "judged"],
        "{lines:#?}"
    );
}

/// With no hold the head says so, and each rule is quiet.
#[test]
fn why_with_no_hold_says_none() {
    let mut s = said(
        Occupancy::Prompt,
        [SaidVerdict::Quiet, SaidVerdict::Quiet],
        None,
    );
    s.hold = None;
    s.rules[0].at_ms = 0;
    s.rules[1].at_ms = 0;
    let lines = why_lines(&s, s.at_ms, 0);
    let head: Vec<&str> = lines[0].split_whitespace().collect();
    assert_eq!(head, ["occupancy=prompt", "hold", "none"]);
    assert_eq!(s.hold_word(), "none");
    for line in &lines[1..3] {
        assert_eq!(line.split_whitespace().nth(2), Some("quiet:"), "{line}");
    }
}

#[test]
fn spans_and_clocks_are_spelled_plainly() {
    for (ms, words) in [
        (45_000, "45s"),
        (20 * MIN, "20m"),
        (5 * HOUR + 25 * MIN, "5h25m"),
        (2 * HOUR, "2h00m"),
        (-5, "0s"),
    ] {
        assert_eq!(span_words(ms), words, "{ms}");
    }
    assert_eq!(clock_words(SINCE, 0), "18:52");
    assert_eq!(clock_words(SINCE, -7 * 3600), "11:52");
    assert_eq!(clock_words(SINCE, 6 * 3600), "00:52");
    assert!(cap_words(RuleId::WaitNudge).starts_with(&rules::NUDGES_PER_DAY.to_string()));
    assert!(cap_words(RuleId::HoldRoom).starts_with(&rules::HOLD_ROOM_PER_HOLD.to_string()));
}

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

/// A told word of slice 1's two rules (a recording without slice 2's doors carries those alone).
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
        rules: vec![
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
        ready_idle: None,
        wake: None,
    }
}

/// [`said`] with slice 2's wake rule after them, saying `verdict` of wake #2.
fn said_wake(verdict: SaidVerdict, guards: Option<Guards>) -> OccupancySaid {
    let mut s = said(
        Occupancy::Observe,
        [SaidVerdict::Outranked, SaidVerdict::Quiet],
        guards,
    );
    s.rules.push(SaidRule {
        rule: RuleId::Wake,
        verdict,
        at_ms: SINCE + HOUR,
    });
    s.wake = Some(SaidWake {
        ord: 2,
        since_ms: SINCE,
    });
    s
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

/// THE TOLD WORD'S RULES PRINT AS SLICE 1'S ARRAY DID (the slice-2 plan's D7): a tape compares the
/// word's `Debug`, and the list's is the two-rule array's byte for byte, in both forms, so a
/// slice-1 tape's tells replay unchanged. A recording without slice 2's doors carries slice 1's
/// rules alone, and one with them `wake@v1` too. NEGATIVE CONTROL: a list of one rule prints
/// otherwise.
#[test]
fn the_told_rules_print_as_slice_ones_array() {
    let s = said(
        Occupancy::Prompt,
        [SaidVerdict::Typed, SaidVerdict::Spent],
        None,
    );
    let slice1: [SaidRule; 2] = [s.rules[0], s.rules[1]];
    assert_eq!(format!("{:?}", s.rules), format!("{slice1:?}"));
    assert_eq!(format!("{:#?}", s.rules), format!("{slice1:#?}"));
    let told: Vec<RuleId> = s.rules.iter().map(|r| r.rule).collect();
    assert_eq!(told_rules(false), told);
    assert_eq!(told, [RuleId::WaitNudge, RuleId::HoldRoom]);
    assert_eq!(told_rules(true), RuleId::ALL);
    assert_eq!(
        RuleId::ALL
            .into_iter()
            .filter(|r| !r.slice1())
            .collect::<Vec<_>>(),
        [RuleId::Wake]
    );
    let one = vec![s.rules[0]];
    assert_ne!(format!("{one:?}"), format!("{slice1:?}"));
}

/// A line of the hold due past K looks.
fn raised() -> SaidReadyIdle {
    SaidReadyIdle {
        rule: RuleId::WaitNudge,
        guard: Some(Guard::Draft),
        since_ms: SINCE + 30 * MIN,
    }
}

/// THE READY-IDLE LINE IS THE ENTRY'S UNDER `prompt` ALONE, with a hold open (the plan's W4, the
/// loop's half): the word names it under every mode, and only `prompt` hands it on. NEGATIVE
/// CONTROLS: `observe`, `off` and a closed hold hand on nothing; a word with none hands on none.
#[test]
fn the_ready_idle_line_is_named_under_prompt_alone() {
    let word = |mode: Occupancy| {
        let mut s = said(
            mode,
            [SaidVerdict::Held(Guard::Draft), SaidVerdict::Quiet],
            None,
        );
        s.ready_idle = Some(raised());
        s
    };
    assert_eq!(
        word(Occupancy::Prompt).ready_idle_under_prompt(),
        Some(raised())
    );
    for mode in [Occupancy::Observe, Occupancy::Off] {
        assert_eq!(word(mode).ready_idle_under_prompt(), None, "{mode:?}");
    }
    let mut closed = word(Occupancy::Prompt);
    closed.hold = None;
    assert_eq!(closed.ready_idle_under_prompt(), None, "no hold");
    let mut none = word(Occupancy::Prompt);
    none.ready_idle = None;
    assert_eq!(none.ready_idle_under_prompt(), None);
    assert!(
        !none.same_change(&word(Occupancy::Prompt)),
        "its raise is a change"
    );
}

/// The told word as slice 1 derived its `Debug`, field for field.
#[derive(Debug)]
#[expect(dead_code, reason = "read through its Debug alone")]
struct SliceOneSaid {
    mode: Occupancy,
    hold: Option<SaidHold>,
    ready: bool,
    guards: Option<Guards>,
    rules: [SaidRule; 2],
    at_ms: i64,
    reading: OccupancyReading,
}

/// A WORD WITH NO READY-IDLE LINE PRINTS AS SLICE 1'S DID (the plan's D7): a tape compares the told
/// word's `Debug`, so a word that raises nothing is slice 1's bytes in both forms, and only a raise
/// prints the field. NEGATIVE CONTROL: a raised word prints otherwise.
#[test]
fn a_word_with_no_ready_idle_line_prints_as_slice_ones() {
    let s = said(
        Occupancy::Prompt,
        [SaidVerdict::Held(Guard::Draft), SaidVerdict::Spent],
        Some(holding(&[Guard::Draft])),
    );
    let slice1 = SliceOneSaid {
        mode: s.mode,
        hold: s.hold,
        ready: s.ready,
        guards: s.guards,
        rules: [s.rules[0], s.rules[1]],
        at_ms: s.at_ms,
        reading: s.reading,
    };
    let renamed = |text: String| text.replacen("SliceOneSaid", "OccupancySaid", 1);
    assert_eq!(format!("{s:?}"), renamed(format!("{slice1:?}")));
    assert_eq!(format!("{s:#?}"), renamed(format!("{slice1:#?}")));
    let mut up = s.clone();
    up.ready_idle = Some(raised());
    let text = format!("{up:?}");
    assert_ne!(text, format!("{s:?}"));
    assert!(
        text.ends_with(&format!(", ready_idle: {:?} }}", raised())),
        "{text}"
    );
}

/// F14'S WAKE LINE: a wake a guard holds says `wake #<n> held: <guard> (since …; K at …)`, K the
/// window's K looks past its condition holding; one that would go names its ordinal and since; and
/// an outranked rule says so. NEGATIVE CONTROL: with no wake judged the wake rule says none ready.
#[test]
fn why_says_the_wake_held_and_since_when() {
    let held = said_wake(
        SaidVerdict::Held(Guard::Draft),
        Some(holding(&[Guard::Draft])),
    );
    let lines = why_lines(&held, held.at_ms, 0);
    assert_eq!(lines.len(), 5, "{lines:#?}");
    let k_at = clock_words(SINCE + K_LOOKS * LOOK_EVERY_MS, 0);
    assert_eq!(
        lines[3],
        format!("wake #2 held: draft (since 18:52; K at {k_at})"),
        "{lines:#?}"
    );
    assert!(lines[1].contains(" outranked "), "{lines:#?}");
    let would = said_wake(SaidVerdict::Would, None);
    let lines = why_lines(&would, would.at_ms, 0);
    assert!(
        lines[3].starts_with("wake@v1 rev=builtin would ")
            && lines[3].ends_with(": wake #2 since 18:52"),
        "{lines:#?}"
    );
    let mut none = would.clone();
    none.wake = None;
    let lines = why_lines(&none, none.at_ms, 0);
    assert!(lines[3].ends_with(": no wake ready"), "{lines:#?}");
}

/// THE WAKE IS PART OF THE CHANGE and prints only when judged: a word without it prints as one
/// before slice 2 did. NEGATIVE CONTROL: a judged wake prints its field last.
#[test]
fn a_judged_wake_is_a_change_and_prints_last() {
    let s = said_wake(SaidVerdict::Would, None);
    let mut other = s.clone();
    other.wake = Some(SaidWake {
        ord: 3,
        since_ms: SINCE,
    });
    assert!(!s.same_change(&other));
    let text = format!("{s:?}");
    assert!(
        text.ends_with(&format!(", wake: {:?} }}", s.wake.expect("judged"))),
        "{text}"
    );
    let mut bare = s.clone();
    bare.wake = None;
    assert!(!format!("{bare:?}").contains("wake: "), "{bare:?}");
    assert_eq!(SaidVerdict::Outranked.words(), "outranked");
    assert_eq!(cap_words(RuleId::Wake), "1 line a wake");
}

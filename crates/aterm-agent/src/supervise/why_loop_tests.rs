// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use astream_effects::EffectRecord;

use crate::supervise::policy::approval::{RULE_ALLOW_ONCE, RULE_READ_ONLY};
use crate::supervise::tape::codec::Item;
use crate::supervise::tape::core::DecideMark;
use crate::supervise::tape::doors;
use crate::supervise::tape::explain::{At, Format, Why, render, why};
use crate::supervise::tape::recover::recover;
use crate::supervise::tape::replay::{Options, replay_traced};
use crate::supervise::tape::testing::written;

fn script() -> Mock {
    let mut m = fenced(vec![
        busy_screen(),
        bash_one_row(),
        busy_screen(),
        question_screen(),
        busy_screen(),
        idle_screen(),
    ]);
    m.vanish_after = Some(3);
    m
}

fn recorded() -> (String, Vec<Item>) {
    let mut m = script();
    let run = record_watch(&mut m, &auto(30), |_| {});
    let header = run.header.encode();
    let read = recover(&run.tape.reframed(&header));
    (header, read.items)
}

fn no_zone(unix: i64) -> Option<i64> {
    panic!("the local zone read for a point named by no local time (at {unix})")
}

fn utc(_: i64) -> Option<i64> {
    Some(0)
}

fn explained(header: &str, items: &[Item], at: &At) -> Why {
    why(&written(header, items), at, &no_zone).unwrap_or_else(|e| panic!("why at {at:?}: {e}"))
}

fn crosses(item: &Item, door: &str) -> bool {
    matches!(item.record(), Some(EffectRecord::Exchange { door: d, .. }) if d == door)
}

#[test]
fn why_explains_a_recorded_approval() {
    let (header, items) = recorded();
    let w = explained(&header, &items, &At::Seq(102));
    assert_eq!(w.door, doors::DECIDE_APPROVAL);
    assert_eq!(w.seq, 102);
    assert_eq!(w.screen.as_deref(), Some(bash_one_row().as_slice()));
    let parsed = w.parsed.as_ref().expect("a box read");
    assert_eq!(parsed.kind, "bash", "{parsed:?}");
    assert!(
        parsed.options.iter().any(|o| o.contains("1. Yes")),
        "{parsed:?}"
    );
    assert_eq!(w.recorded_rule.as_deref(), Some(RULE_READ_ONLY));
    assert_eq!(
        w.agrees,
        Some(true),
        "{:?} vs {}",
        w.decided,
        w.recorded_outcome
    );
    assert_eq!(w.decided_by(), Some(RULE_READ_ONLY), "{:#?}", w.trace);
    assert!(w.needs.is_empty(), "{:?}", w.needs);
    assert!(
        w.act
            .as_ref()
            .is_some_and(|a| a.first().map(String::as_str) == Some("key")
                && a.last().map(String::as_str) == Some("1")),
        "{:?}",
        w.act
    );
    assert!(w.reply.is_some(), "{w:#?}");
    assert_eq!(
        w.journal.as_deref(),
        Some("APPROVED seq=102 git log --oneline -5")
    );
    let fact = |name: &str| {
        w.facts
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("fact {name}: {:#?}", w.facts))
    };
    assert!(
        fact("worker").value.starts_with("digest "),
        "{:#?}",
        w.facts
    );
    assert!(
        fact("screen").door.starts_with("ctl text #"),
        "{:#?}",
        w.facts
    );
    assert_eq!(w.recorded_by.as_deref(), Some(w.explained_by.as_str()));
    for format in [Format::Text, Format::Md, Format::Html] {
        assert!(render(&w, format).contains(RULE_READ_ONLY), "{format:?}");
    }
}

#[test]
fn why_finds_a_decision_by_door_and_by_line() {
    let (header, items) = recorded();
    let by_seq = explained(&header, &items, &At::Seq(102));
    let by_door = explained(&header, &items, &At::Door(by_seq.mark + 2));
    assert_eq!(by_door, by_seq);
    let by_line = explained(&header, &items, &At::Line("APPROVED seq=102".to_string()));
    assert_eq!(by_line, by_seq);
    assert!(why(&written(&header, &items), &At::Seq(9_999), &no_zone).is_err());
    assert!(why(&written(&header, &items), &At::Door(0), &no_zone).is_err());
}

#[test]
fn a_fact_the_recording_build_did_not_write_is_named_in_needs() {
    let (header, mut items) = recorded();
    let mut edited = 0;
    for item in &mut items {
        if let Item::Effect {
            record: EffectRecord::Emit { door, bytes },
            ..
        } = item
            && door == doors::DECIDE_APPROVAL
            && let Some(mut mark) = std::str::from_utf8(bytes).ok().and_then(DecideMark::decode)
        {
            let facts = mark.facts.take().expect("an approval's facts");
            let at = facts.find("\"screen_cols\":").expect("the field");
            let end = at + facts[at..].find(',').expect("a field after it") + 1;
            mark.facts = Some(format!("{}{}", &facts[..at], &facts[end..]));
            *bytes = mark.encode().into_bytes();
            edited += 1;
        }
    }
    assert_eq!(edited, 1, "the one approval's mark");
    let w = explained(&header, &items, &At::Seq(102));
    assert_eq!(w.needs, vec!["fact screen_cols".to_string()]);
    assert!(w.trace.is_some(), "decided again all the same");
}

#[test]
fn a_look_the_tape_does_not_hold_is_named_and_decided_cautiously() {
    let (header, items) = recorded();
    let with = explained(&header, &items, &At::Seq(102));
    assert_eq!(with.agrees, Some(true));
    let without: Vec<Item> = items
        .into_iter()
        .filter(|i| !crosses(i, doors::PROBE_GIT_VIEW))
        .collect();
    let w = explained(&header, &without, &At::Seq(102));
    decided_cautiously_without_the_look(&w);
}

fn decided_cautiously_without_the_look(w: &Why) {
    assert!(
        w.needs.iter().any(|n| n.starts_with("look probe.git_view")),
        "{:?}",
        w.needs
    );
    assert!(
        w.decided
            .as_deref()
            .is_some_and(|d| d.starts_with("Escalate")),
        "the cautious answer: {:?}",
        w.decided
    );
    assert_eq!(w.agrees, None, "{:?}", w.decided);
}

#[test]
fn a_look_recorded_after_the_point_is_not_an_answer() {
    let (header, mut items) = recorded();
    let look = items
        .iter()
        .position(|i| crosses(i, doors::PROBE_GIT_VIEW))
        .expect("the approval's git read");
    let moved = items.remove(look);
    let mark = items
        .iter()
        .position(|i| {
            matches!(i.record(), Some(EffectRecord::Emit { door, .. })
                if door == doors::DECIDE_APPROVAL)
        })
        .expect("the approval's mark");
    assert!(look <= mark, "the look was before the mark");
    items.insert(mark + 1, moved);
    let w = explained(&header, &items, &At::Seq(102));
    decided_cautiously_without_the_look(&w);
}

#[test]
fn a_turn_ends_trace_is_computed_in_a_replay_on_the_recording_build() {
    let (header, items) = recorded();
    let w = explained(&header, &items, &At::Seq(104));
    assert_eq!(w.door, doors::DECIDE_TURN_END);
    assert!(!w.notes.is_empty(), "the recorder's note of the fold");
    let steps = w
        .trace
        .as_ref()
        .unwrap_or_else(|| panic!("{:?}", w.trace_note));
    assert!(!steps.is_empty());
    assert!(w.decided_by().is_some(), "{steps:#?}");
    assert!(
        steps.iter().any(|s| s.check.starts_with("pass:")),
        "each pass of the point named: {steps:#?}"
    );
    let bytes = written(&header, &items);
    let to_mark = Options {
        until: Some(w.mark),
        ..Options::default()
    };
    let (plain, none) = replay_traced(&bytes, &to_mark).expect("replays");
    assert!(none.is_none(), "{none:#?}");
    let (traced, kept) = replay_traced(
        &bytes,
        &Options {
            trace_at: Some(w.mark),
            ..to_mark
        },
    )
    .expect("replays");
    assert_eq!(
        traced, plain,
        "a kept trace changes nothing the replay does"
    );
    assert_eq!(kept.as_ref(), w.trace.as_ref());
    let other: String = header
        .lines()
        .map(|l| {
            if l.starts_with("build=") {
                "build=g0000000-other".to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert_ne!(other, header, "the header names its build");
    let w = explained(&other, &items, &At::Seq(104));
    assert!(w.trace.is_none());
    assert!(
        w.trace_note
            .as_deref()
            .is_some_and(|n| n.contains("g0000000-other")),
        "{:?}",
        w.trace_note
    );
}

#[test]
fn a_header_this_build_cannot_read_still_explains_what_the_tape_holds() {
    let (header, items) = recorded();
    let control = explained(&header, &items, &At::Seq(102));
    assert!(control.trace.is_some());
    let (_, body) = header.split_once('\n').expect("a header's first line");
    let unread = format!("atape 999\n{body}");
    let w = explained(&unread, &items, &At::Seq(102));
    assert_eq!(w.door, doors::DECIDE_APPROVAL);
    assert_eq!(w.screen, control.screen);
    assert_eq!(w.act, control.act);
    assert_eq!(w.journal, control.journal);
    assert_eq!(w.recorded_by, control.recorded_by);
    assert_eq!(w.tape, control.tape);
    assert!(w.trace.is_none() && w.agrees.is_none(), "{w:#?}");
    assert!(
        w.trace_note
            .as_deref()
            .is_some_and(|n| n.contains("does not read on this build")),
        "{:?}",
        w.trace_note
    );
}

fn marked(items: &[Item], door: &str) -> Vec<u64> {
    items
        .iter()
        .filter_map(|i| match i.record() {
            Some(EffectRecord::Emit { door: d, bytes }) if d == door => {
                std::str::from_utf8(bytes).ok().and_then(DecideMark::decode)
            }
            _ => None,
        })
        .map(|m| m.seq)
        .collect()
}

#[test]
fn a_policy_key_the_header_lacks_is_named_as_supplied_by_this_build() {
    let (header, items) = recorded();
    let control = explained(&header, &items, &At::Seq(102));
    assert!(control.needs.is_empty(), "{:?}", control.needs);
    let older: String = header
        .lines()
        .filter(|l| !l.starts_with("approve="))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_ne!(older, header, "PRECONDITION: the header says approve");
    let w = explained(&older, &items, &At::Seq(102));
    assert!(
        w.needs
            .iter()
            .any(|n| n == "supplied by this build: [policy] approve"),
        "{:?}",
        w.needs
    );
    assert!(
        w.trace_note
            .as_deref()
            .is_some_and(|n| n.contains("with what this build supplied")),
        "{:?}",
        w.trace_note
    );
}

#[test]
fn a_turn_end_cut_short_leaves_no_steps_in_the_next_ones_trace() {
    let mut m = script();
    // The first busy wait's `status` (the session log's head) moves every later request one
    // on: request 16 is the one the turn end at 104 loses.
    m.by_index.insert(16, closed());
    let run = record_watch(&mut m, &auto(30), |_| {});
    let header = run.header.encode();
    let items = recover(&run.tape.reframed(&header)).items;
    let ends = marked(&items, doors::DECIDE_TURN_END);
    assert!(
        !ends.contains(&104) && ends.contains(&106),
        "PRECONDITION: the turn end at 104 cut short, the one at 106 marked: {ends:?}"
    );
    let w = explained(&header, &items, &At::Seq(106));
    let steps = w
        .trace
        .as_ref()
        .unwrap_or_else(|| panic!("{:?}", w.trace_note));
    assert_eq!(
        steps.iter().filter(|s| s.check == "pass:cruise").count(),
        1,
        "{steps:#?}"
    );
    assert_eq!(steps.first().map(|s| s.check), Some("pass:cruise"));
}

#[test]
fn a_mark_with_no_facts_blames_no_cause_its_outcome_does_not_name() {
    let (header, mut items) = recorded();
    let latch = "Escalate { reason: \"the question policy was recommended at the supervisor's \
                 first key on this dialog and is ask now: a person answers it\" }";
    for item in &mut items {
        if let Item::Effect {
            record: EffectRecord::Emit { door, bytes },
            ..
        } = item
            && door == doors::DECIDE_APPROVAL
            && let Some(mut mark) = std::str::from_utf8(bytes).ok().and_then(DecideMark::decode)
        {
            mark.facts = None;
            mark.rule = None;
            mark.outcome = latch.to_string();
            *bytes = mark.encode().into_bytes();
        }
    }
    let w = explained(&header, &items, &At::Seq(102));
    assert!(w.trace.is_none() && w.agrees.is_none(), "{w:#?}");
    let note = w.trace_note.as_deref().unwrap_or_default();
    assert!(
        !note.contains("another supervisor") && !note.contains("approve = \"none\""),
        "{note}"
    );
    assert!(note.contains("the reason recorded"), "{note}");
}

fn walls(items: &mut [Item], split: usize, before: i64, after: i64) {
    for (i, item) in items.iter_mut().enumerate() {
        let ms = if i <= split { before } else { after };
        if let Item::Effect { record, stamp } = item {
            if let EffectRecord::Clock(ns) = record {
                *ns = u64::try_from(ms).expect("after 1970") * 1_000_000;
            }
            if let Some(s) = stamp {
                s.wall_ms = ms;
            }
        }
    }
}

fn approval_mark(items: &[Item]) -> usize {
    items
        .iter()
        .position(|i| {
            matches!(i.record(), Some(EffectRecord::Emit { door, .. })
                if door == doors::DECIDE_APPROVAL)
        })
        .expect("the approval's mark")
}

const LATE: i64 = 1_792_454_100_000;
const EARLY: i64 = LATE + 600_000;

#[test]
fn a_local_time_names_an_instant_on_a_tape_that_runs_past_midnight() {
    let (header, mut items) = recorded();
    let split = approval_mark(&items);
    walls(&mut items, split, LATE, EARLY);
    let bytes = written(&header, &items);
    let at = |text: &str| why(&bytes, &At::parse(text).expect("reads"), &utc);
    for text in ["23:58", "00:01", "2026-10-20T00:01", "2026-10-19 23:59:59"] {
        let w = at(text).unwrap_or_else(|e| panic!("--at {text}: {e}"));
        assert_eq!(
            (w.door.as_str(), w.seq, w.at_ms),
            (doors::DECIDE_APPROVAL, 102, Some(LATE)),
            "--at {text}"
        );
        assert_eq!(w.at_local.as_deref(), Some("2026-10-19 23:55:00"));
        assert!(
            render(&w, Format::Text)
                .contains("made 2026-10-19T23:55:00Z (2026-10-19 23:55:00 local)"),
            "{}",
            render(&w, Format::Text)
        );
    }
    let last = at("2026-10-20T00:06").expect("the last decision");
    assert_eq!(last.at_ms, Some(EARLY));
    assert!(last.seq > 102, "{last:?}");
    for text in ["12:00", "00:06"] {
        let err = at(text).expect_err("inside the run on neither day");
        assert!(
            err.contains("name the day") && err.contains("2026-10-19 23:55:00"),
            "{err}"
        );
    }
    assert_eq!(at("2026-10-20T12:00").map(|w| w.seq), Ok(last.seq));
    assert!(at("2026-10-19T12:00").is_err(), "before every decision");
}

#[test]
fn a_decisions_local_time_is_read_by_its_own_instants_offset() {
    const BEFORE: i64 = 1_793_505_600_000;
    const AFTER: i64 = BEFORE + 9_000_000;
    const CHANGE: i64 = 1_793_512_800;
    let (header, mut items) = recorded();
    let split = approval_mark(&items);
    walls(&mut items, split, BEFORE, AFTER);
    let bytes = written(&header, &items);
    let asked = std::cell::RefCell::new(Vec::new());
    let zone = |unix: i64| {
        asked.borrow_mut().push(unix);
        Some(if unix < CHANGE { -4 * 3600 } else { -5 * 3600 })
    };
    let w = why(&bytes, &At::parse("01:45").expect("reads"), &zone).expect("a decision");
    assert_eq!(w.at_ms, Some(AFTER), "{w:?}");
    assert_eq!(w.at_local.as_deref(), Some("2026-11-01 01:30:00"));
    let w = why(&bytes, &At::parse("01:15").expect("reads"), &zone).expect("a decision");
    assert_eq!((w.seq, w.at_ms), (102, Some(BEFORE)), "{w:?}");
    assert!(
        asked
            .borrow()
            .iter()
            .all(|&u| u == BEFORE / 1000 || u == AFTER / 1000),
        "{:?}",
        asked.borrow()
    );
}

fn breaker_recorded(off: usize, spoil: impl FnOnce(&mut Mock)) -> (String, Vec<Item>) {
    use aterm_phase::prompt::fixtures as boxes;
    let rows = boxes::screen(boxes::BOX_RM_BREAKER_2_1_287);
    let rule = rows
        .iter()
        .rposition(|r| aterm_phase::phase::is_rule(r))
        .expect("the box's top rule");
    let mut m = fenced(vec![rows[rule + off..].to_vec(), idle_screen()]);
    m.key_releases = Some(0);
    m.alt_screen = Some(false);
    let mut history = boxes::screen(boxes::BOX_READ_FORCED_ACCEPT_EDITS_2_1_292);
    history.extend(rows[..rule + off].iter().cloned());
    m.scrollback = Some(history);
    m.vanish_after = Some(2);
    spoil(&mut m);
    let mut opts = auto(30);
    opts.policy.approve = Approve::All;
    opts.policy.rm_breaker_ask = false;
    let run = record_watch(&mut m, &opts, |_| {});
    let header = run.header.encode();
    (header.clone(), recover(&run.tape.reframed(&header)).items)
}

#[test]
fn a_box_head_read_from_the_history_is_rebuilt_from_the_tape() {
    let (header, items) = breaker_recorded(8, |_| {});
    let seq = *marked(&items, doors::DECIDE_APPROVAL)
        .first()
        .expect("the box's mark");
    let w = explained(&header, &items, &At::Seq(seq));
    assert_eq!(w.recorded_rule.as_deref(), Some(RULE_ALLOW_ONCE));
    assert!(w.needs.is_empty(), "{:?}", w.needs);
    assert_eq!(
        w.agrees,
        Some(true),
        "{:?} vs {}",
        w.decided,
        w.recorded_outcome
    );
    assert!(
        w.facts
            .iter()
            .any(|f| f.name == "box_head" && f.door.starts_with("ctl search #")),
        "{:#?}",
        w.facts
    );
    let first_line = items
        .iter()
        .position(|i| match i.record() {
            Some(EffectRecord::Exchange { door, request, .. }) if door == doors::CTL => {
                crate::supervise::tape::codec::ctl_args(request)
                    .is_ok_and(|w| w.iter().any(|w| w == "line"))
            }
            _ => false,
        })
        .expect("a head row's read");
    let mut short = items.clone();
    short.remove(first_line);
    let w = explained(&header, &short, &At::Seq(seq));
    assert!(
        w.needs
            .iter()
            .any(|n| n.starts_with("the box's head off the screen")),
        "{:?}",
        w.needs
    );
    assert_eq!(w.agrees, Some(false), "{:?}", w.decided);
    type Spoil = fn(&mut Mock);
    let controls: [(&str, Spoil, bool); 3] = [
        (
            "a transcript row under the rule",
            |m| {
                if let Some(h) = m.scrollback.as_mut() {
                    let at = h.len() - 2;
                    h.insert(at, "⏺ Bash(rm -rf /)".to_string());
                }
            },
            false,
        ),
        (
            "the box changed under the reads",
            |m| {
                m.key_releases = None;
                let other = m.screens[0]
                    .iter()
                    .map(|r| {
                        if r == "   2. No" {
                            "   2. No, and tell Claude what to do differently".to_string()
                        } else {
                            r.clone()
                        }
                    })
                    .collect();
                m.screens.insert(1, other);
            },
            false,
        ),
        (
            "the generation moving",
            |m| {
                m.key_releases = None;
                m.screens.truncate(1);
            },
            true,
        ),
    ];
    for (what, spoil, joined) in controls {
        let (header, items) = breaker_recorded(8, spoil);
        let seq = *marked(&items, doors::DECIDE_APPROVAL)
            .first()
            .unwrap_or_else(|| panic!("{what}: the box's mark"));
        assert!(
            items.iter().any(|i| matches!(i.record(),
                Some(EffectRecord::Exchange { door, request, .. })
                    if door == doors::CTL
                        && String::from_utf8_lossy(request).contains("search"))),
            "PRECONDITION {what}: the loop searched for the head"
        );
        let w = explained(&header, &items, &At::Seq(seq));
        assert!(w.needs.is_empty(), "{what}: {:?}", w.needs);
        assert_eq!(
            w.agrees,
            Some(true),
            "{what}: {:?} vs {}",
            w.decided,
            w.recorded_outcome
        );
        assert_eq!(
            w.facts.iter().any(|f| f.name == "box_head"),
            joined,
            "{what}: {:#?}",
            w.facts
        );
    }
}

#[test]
fn the_runs_python_globs_reach_the_decision_made_again() {
    let mut m = fenced(vec![
        busy_screen(),
        bash_box(&["⏺ Reporting."], "python3 tools/report.py"),
        busy_screen(),
        idle_screen(),
    ]);
    m.vanish_after = Some(2);
    let mut opts = auto(30);
    opts.python_allow = vec!["tools/*.py".to_string()];
    let run = record_watch(&mut m, &opts, |_| {});
    let header = run.header.encode();
    let items = recover(&run.tape.reframed(&header)).items;
    let seq = *marked(&items, doors::DECIDE_APPROVAL)
        .first()
        .expect("the box's mark");
    let w = explained(&header, &items, &At::Seq(seq));
    assert_eq!(
        w.recorded_rule.as_deref(),
        Some(RULE_READ_ONLY),
        "{}",
        w.recorded_outcome
    );
    assert_eq!(w.agrees, Some(true), "{:?}", w.decided);
    let globless: String = header
        .lines()
        .filter(|l| !l.starts_with("python_allow."))
        .map(|l| {
            if l.starts_with("python_allow=") {
                "python_allow=0\n".to_string()
            } else {
                format!("{l}\n")
            }
        })
        .collect();
    assert_ne!(
        globless, header,
        "PRECONDITION: the header carries the glob"
    );
    let w = explained(&globless, &items, &At::Seq(seq));
    assert_eq!(w.agrees, Some(false), "{:?}", w.decided);
}

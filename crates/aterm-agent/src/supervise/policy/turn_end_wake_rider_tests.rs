// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::{BTreeMap, BTreeSet};

use crate::supervise::config::Occupancy;
use crate::supervise::facts::{Fact, FactKind};
use crate::supervise::occupancy::FactLine;
use crate::supervise::occupancy::templates::{self, LINE_CAP};
use crate::supervise::occupancy::wakes_seen::{AtomKind, Declarer};

/// Wake `ord`'s line, a job's (`pid:` exited), ready since 23:40.
fn wake_line(ord: u16) -> FactLine {
    templates::wake(ord, Declarer::Agent, &[AtomKind::Pid], Some((23, 40)), None)
}

/// Wake `ord`'s fact as a point's decision is handed it under the owner's `prompt`.
fn ride(ord: u16) -> RideFact {
    ride_of(&wake_line(ord), ord)
}

/// The fact of wake `ord`'s `line` as a point's decision is handed it under `prompt`.
fn ride_of(line: &FactLine, ord: u16) -> RideFact {
    let grant = Occupancy::Prompt.grant().expect("prompt grants");
    RideFact::new(grant, line, ord, 0xfeed_0000 + u64::from(ord), 0)
}

/// The decision `plain` with `ride` after it where the whole stays under [`LINE_CAP`] (the
/// slice-2 plan's F9), else `plain` as it is: the fact waits whole.
fn ridden(plain: &TurnEndAction, ride: &RideFact) -> TurnEndAction {
    let said = format!("{}{}", text_of(plain), fact_suffix(ride.fact()));
    match plain {
        TurnEndAction::Type { rule_id, .. } if said.chars().count() < LINE_CAP => {
            TurnEndAction::Type {
                text: said,
                rule_id,
            }
        }
        _ => plain.clone(),
    }
}

fn carrying(r: &TurnEndReading, ride: &RideFact) -> TurnEndReading {
    TurnEndReading {
        wake: Some(ride.clone()),
        ..r.clone()
    }
}

/// The decision at `r` from `st`, as it is and with `ride` handed it: neither moves `st`.
fn both(
    st: &TurnEndState,
    r: &TurnEndReading,
    ride: &RideFact,
    now: Instant,
) -> [TurnEndAction; 2] {
    let decide = |r: &TurnEndReading| {
        let mut st = st.clone();
        st.observe(r, now);
        decide_turn_end(&st, r, &cfg(), now)
    };
    [decide(r), decide(&carrying(r, ride))]
}

fn text_of(a: &TurnEndAction) -> &str {
    match a {
        TurnEndAction::Type { text, .. } => text,
        other => panic!("no text: {other:?}"),
    }
}

/// A DECLARED WAKE'S FACT RIDES EACH OF THE FOUR ACTS WHOLE AND THE ACT STILL READS AS ITSELF (the
/// slice-2 plan's 2.9, D3): the continuation, the done check, Land's instruction and its nudge
/// each carry ` (aterm: <the wake's fact sentence>)` after the act's own words, once, wherever
/// the whole stays under [`LINE_CAP`], and are typed as they were where it would not (a wake's
/// fact with its clock, or one without it, each where it fits); each act rides one, and each
/// ridden text is still read as its act (the done check's and Land's recognizers; the
/// continuation opens with its own words). The fact is the template's sentence alone: no part of
/// its line's mark, advice or rule name rides with it. NEGATIVE CONTROL: the same decisions
/// handed no wake carry nothing.
#[test]
fn a_wakes_fact_rides_each_of_the_four_acts_whole_and_each_still_reads_as_its_act() {
    let clocked = ride(1);
    let bare = ride_of(
        &templates::wake(1, Declarer::Agent, &[AtomKind::Pid], None, None),
        1,
    );
    for (ride, line) in [
        (&clocked, wake_line(1)),
        (
            &bare,
            templates::wake(1, Declarer::Agent, &[AtomKind::Pid], None, None),
        ),
    ] {
        let suffix = fact_suffix(ride.fact());
        let sentence = line.fact().strip_suffix('.').expect("a sentence");
        assert_eq!(ride.fact().text, sentence);
        assert_eq!(ride.fact().key.kind, FactKind::Wake);
        let (head, tail) = line
            .text()
            .split_once(line.fact())
            .expect("the line holds its fact");
        for part in [head.trim(), tail.trim()] {
            assert!(
                !part.is_empty() && !suffix.contains(part),
                "{part:?} in {suffix:?}"
            );
        }
    }
    let mut rode = Vec::new();
    for (mode, first, then) in [
        (Autopilot::Cruise, RULE_CONTINUE, RULE_DONE_CHECK),
        (Autopilot::Land, RULE_LAND, RULE_LAND_NUDGE),
    ] {
        let st = TurnEndState::default();
        let now = t0();
        let r = under(mode, idle("Stage 1 landed.", Some(3 * MIN)));
        let done = under(mode, idle(FINISHED_REPORT, Some(3 * MIN)));
        for ride in [&clocked, &bare] {
            let [plain, with] = both(&st, &r, ride, now);
            assert_eq!(plain.rule_id(), Some(first), "{mode:?}: {plain:?}");
            assert_eq!(with, ridden(&plain, ride), "{mode:?}");
            assert!(!carries_fact(&plain, ride.fact()), "CONTROL: {plain:?}");
            assert_eq!(without_fact(with.clone(), ride.fact()), plain);
            let mut st = st.clone();
            st.observe(&carrying(&r, ride), now);
            st.acted(&with, &carrying(&r, ride), now);
            if carries_fact(&with, ride.fact()) {
                st.fact_rode(ride.fact());
                rode.push(with);
            }
            let later = now + 3 * MIN;
            let (plain, _) = past_backoff(&mut st.clone(), &done, later);
            let (with, _) = past_backoff(&mut st.clone(), &carrying(&done, ride), later);
            assert_eq!(plain.rule_id(), Some(then), "{mode:?}: {plain:?}");
            assert_eq!(with, ridden(&plain, ride), "{mode:?}");
            if carries_fact(&with, ride.fact()) {
                rode.push(with);
            }
        }
    }
    let rules: BTreeSet<&str> = rode.iter().filter_map(TurnEndAction::rule_id).collect();
    assert_eq!(
        rules,
        BTreeSet::from(FACT_RIDES),
        "each act carries a wake's fact"
    );
    for a in &rode {
        let text = text_of(a);
        assert_eq!(text.matches(" (aterm: ").count(), 1, "{text}");
        assert!(text.chars().count() < LINE_CAP, "{text}");
        let reads = match a.rule_id() {
            Some(RULE_CONTINUE) => {
                text.starts_with(&cfg().continue_text)
                    && !is_done_check(text)
                    && !is_land_words(text)
            }
            Some(RULE_DONE_CHECK) => is_done_check(text) && !is_land_words(text),
            Some(RULE_LAND | RULE_LAND_NUDGE) => is_land_words(text) && !is_done_check(text),
            other => panic!("{other:?}"),
        };
        assert!(reads, "{a:?}");
    }
}

/// A WAKE'S FACT NEVER RIDES AN ANSWER, A WALL'S RETRY, A DRAFT, OR AN ACT OF THE UPGRADE'S OR A
/// RELAUNCH'S: each such point decides as it would handed no wake, and no act it decides carries
/// the fact; a relaunch (the memory restart, a model's fallback or its return, an account move)
/// is left as it is, the continuation it would type in its place carrying nothing. (A grey
/// suggestion is no longer accepted bare: the continuation typed over it is a continuation.)
/// NEGATIVE CONTROL: the continuation at a plain point carries it.
#[test]
fn a_wakes_fact_never_rides_an_answer_a_retry_a_draft_or_an_upgrade() {
    let ride = ride(2);
    let now = t0();
    let st = TurnEndState::default();
    let plain = idle("Added the flag; the parser is next.", Some(3 * MIN));
    let [_, control] = both(&st, &plain, &ride, now);
    assert!(carries_fact(&control, ride.fact()), "CONTROL: {control:?}");

    let mut cases: Vec<(&str, TurnEndState, TurnEndReading, Instant)> = vec![(
        "an answer",
        TurnEndState::default(),
        idle("Should I add the flag or the option?", Some(3 * MIN)),
        now,
    )];
    cases.push((
        "a draft",
        TurnEndState::default(),
        TurnEndReading {
            composer: Composer::Typed,
            ..plain.clone()
        },
        now,
    ));
    cases.push((
        "the upgrade's wind-down",
        TurnEndState::default(),
        TurnEndReading {
            upgrading: true,
            ..plain.clone()
        },
        now,
    ));
    // A refused request: the wall the agent can act on, retried after its first rung (an outage's
    // wall is waited out and never typed into since v6's 12b).
    let wall = walled(
        WallKind::ApiError {
            code: Some(400),
            retryable: false,
            cause: aterm_phase::ApiCause::Request,
        },
        "API Error: 400 due to tool use concurrency issues.",
        Some(3 * MIN),
    );
    let mut retrying = TurnEndState::default();
    let first = at(&mut retrying, &wall, now);
    assert!(waits_until(&first, now + MIN), "{first:?}");
    let again = TurnEndReading {
        worked: None,
        ..wall.clone()
    };
    cases.push(("a wall's retry", retrying, again, now + MIN));
    let mut decided = Vec::new();
    for (label, st, r, at) in cases {
        let [plain, with] = both(&st, &r, &ride, at);
        assert_eq!(with, plain, "{label}");
        assert!(!carries_fact(&with, ride.fact()), "{label}: {with:?}");
        decided.push((label, plain));
    }
    let rules: Vec<_> = decided.iter().map(|(l, a)| (*l, a.rule_id())).collect();
    assert!(
        decided
            .iter()
            .any(|(_, a)| a.rule_id().is_some_and(is_answer_rule)),
        "the answer is decided: {rules:?}"
    );
    assert!(
        decided
            .iter()
            .any(|(_, a)| a.rule_id() == Some(RULE_API_RETRY)),
        "the wall is retried: {rules:?}"
    );
    for a in [
        TurnEndAction::Submit {
            rule_id: RULE_CONTINUE,
        },
        TurnEndAction::Type {
            text: "Try again.".to_string(),
            rule_id: RULE_API_RETRY,
        },
    ] {
        assert!(!fact_may_ride(&st, &a), "{a:?}");
    }
    let TurnEndAction::Type { text, rule_id } = &control else {
        panic!("{control:?}");
    };
    let cont = TurnEndAction::Type {
        text: text
            .strip_suffix(&fact_suffix(ride.fact()))
            .expect("its own text")
            .to_string(),
        rule_id,
    };
    for (why, rule_id) in [
        (Restart::Memory, RULE_MEMORY_RESTART),
        (
            Restart::Model {
                to: "sonnet".to_string(),
            },
            RULE_MODEL_FALLBACK,
        ),
        (Restart::ModelBack { to: None }, RULE_MODEL_RESTORE),
        (
            Restart::Account {
                to: "work".to_string(),
                at: AccountAt::Ahead,
            },
            RULE_ACCOUNT,
        ),
    ] {
        let relaunch = TurnEndAction::Restart {
            why,
            rule_id,
            otherwise: Box::new(cont.clone()),
        };
        assert!(!fact_may_ride(&st, &relaunch), "{relaunch:?}");
        assert!(!carries_fact(&relaunch, ride.fact()), "{relaunch:?}");
        assert_eq!(
            fact_rides(&st, relaunch.clone(), Some(ride.fact()), &mut NoTrace),
            relaunch,
            "a relaunch is left as it is"
        );
    }
    assert!(
        carries_fact(
            &fact_rides(&st, cont.clone(), Some(ride.fact()), &mut NoTrace),
            ride.fact()
        ),
        "CONTROL: the continuation alone carries it"
    );
}

/// A WAKE'S FACT THAT WOULD PASS THE LINE'S CAP WAITS (the slice-2 plan's F9): where the act with
/// it would reach [`LINE_CAP`] characters it is left off whole, the act typed as it was, though a
/// hung call's longer fact still rides within the verbatim limit (NEGATIVE CONTROL: the cap is
/// the wake's own); below it the wake's rides.
#[test]
fn a_wakes_fact_over_the_lines_cap_waits_whole() {
    let ride = ride(3);
    let suffix = fact_suffix(ride.fact());
    let hung = Fact {
        key: crate::supervise::facts::EpisodeKey::no_progress(27098, 7),
        text: format!("{} and then some", ride.fact().text),
        as_of_ms: 0,
    };
    let now = t0();
    let st = TurnEndState::default();
    let mut seen = (false, false);
    for n in 1..160 {
        let mut r = idle("Added the flag; the parser is next.", Some(3 * MIN));
        r.rules = Some("word ".repeat(n).trim_end().to_string());
        let [plain, with] = both(&st, &r, &ride, now);
        let over = text_of(&plain).chars().count() + suffix.chars().count() >= LINE_CAP;
        let units =
            text_of(&plain).encode_utf16().count() + fact_suffix(&hung).encode_utf16().count();
        if over {
            assert_eq!(with, plain, "{n} words: waits whole");
            if units <= VERBATIM_UNITS {
                let hung_rides = decide_turn_end(
                    &st,
                    &TurnEndReading {
                        fact: Some(hung.clone()),
                        ..r.clone()
                    },
                    &cfg(),
                    now,
                );
                assert!(
                    carries_fact(&hung_rides, &hung),
                    "{n} words: {hung_rides:?}"
                );
                seen.1 = true;
            }
        } else {
            assert!(carries_fact(&with, ride.fact()), "{n} words: {with:?}");
            seen.0 = true;
        }
    }
    assert_eq!(seen, (true, true), "both sides of the cap were reached");
}

/// THE LINE'S CAP IS A STRICT BOUND (the slice-2 plan's F9: the whole typed text under
/// [`LINE_CAP`]): an act whose text with the wake's fact comes to `LINE_CAP - 1` characters carries
/// it, and one whose text with it comes to `LINE_CAP` exactly is typed without it. Each length is
/// reached (the standing rules grow one character at a time).
#[test]
fn a_wakes_fact_rides_one_character_under_the_lines_cap_and_waits_at_it() {
    let ride = ride(3);
    let suffix = fact_suffix(ride.fact()).chars().count();
    let now = t0();
    let st = TurnEndState::default();
    let mut at_cap = BTreeMap::new();
    for n in 0..LINE_CAP {
        let mut r = idle("Added the flag; the parser is next.", Some(3 * MIN));
        r.rules = Some(format!("Keep {}", "x".repeat(n)));
        let [plain, with] = both(&st, &r, &ride, now);
        let total = text_of(&plain).chars().count() + suffix;
        if total + 1 >= LINE_CAP && total <= LINE_CAP {
            at_cap.insert(total, carries_fact(&with, ride.fact()));
        }
    }
    assert_eq!(
        at_cap,
        BTreeMap::from([(LINE_CAP - 1, true), (LINE_CAP, false)])
    );
}

/// THE ONE FACT SLOT (D3): a hung call's fact rides before a wake's, and a wake's fact rides by
/// its [`RideFact`] alone, which only the owner's grant makes: one of the wake's kind put in the
/// hung call's place rides nothing. `[harness] facts = false` withdraws the wake's as it does the
/// hung call's.
#[test]
fn the_one_slot_takes_a_hung_calls_fact_first_and_a_wakes_only_from_its_grant() {
    let ride = ride(4);
    let now = t0();
    let st = TurnEndState::default();
    let r = idle("Added the flag; the parser is next.", Some(3 * MIN));
    let hung = Fact {
        key: crate::supervise::facts::EpisodeKey::no_progress(27098, 7),
        text: "shell task bplle5dxj (pid 27098) has made no progress for 31 min".to_string(),
        as_of_ms: 0,
    };
    let both_facts = TurnEndReading {
        fact: Some(hung.clone()),
        ..carrying(&r, &ride)
    };
    assert_eq!(slot_fact(&both_facts), Some(&hung));
    let first = decide_turn_end(&st, &both_facts, &cfg(), now);
    assert!(carries_fact(&first, &hung), "{first:?}");
    assert!(!carries_fact(&first, ride.fact()), "{first:?}");
    let [plain, _] = both(&st, &r, &ride, now);
    let unsealed = TurnEndReading {
        fact: Some(wake_fact(&wake_line(4), ride.episode(), 0)),
        ..r.clone()
    };
    assert_eq!(slot_fact(&unsealed), None);
    assert_eq!(decide_turn_end(&st, &unsealed, &cfg(), now), plain);
    let off = SupervisorConfig {
        facts: false,
        ..cfg()
    };
    assert_eq!(decide_turn_end(&st, &carrying(&r, &ride), &off, now), plain);
    assert!(fact_would_ride(&st, &plain, ride.fact()));
    assert!(!fact_would_ride(&st, &TurnEndAction::Nothing, ride.fact()));
}

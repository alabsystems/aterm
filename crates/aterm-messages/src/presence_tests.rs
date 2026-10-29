// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the presence core without a host.

use crate::presence::{
    AgentPhase, AgentReading, ChipLevel, Facts, Hand, HoldFact, Host, Level, Link, MailFacts,
    MailLast, NeedFact, NeedSeverity, Pill, PillAct, PillHit, PillKind, PresenceTones, Rim, Slot,
    StopCause, TurnFact, View, WallAnswer, chip_of_attention, fmt_dur, pill_age, pill_age_room,
    pill_due, pill_layout, pill_text, pill_words, sanitize_token, short_sid, state_line,
};
use crate::{Duration, Instant};

/// A test host: its walls are a usage window, a full context and an API error, its stall is the instant it began, its harness is `harness@1`, its percent codec spells a space `%20`, and its grid is the terminal's own (`aterm_grapheme::str_grid_columns`), counting the characters it measures on this thread ([`MEASURED`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TestHost {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TestWall {
    Usage,
    #[expect(
        dead_code,
        reason = "the host's vocabulary is three kinds; the tests name one"
    )]
    Context,
    #[expect(
        dead_code,
        reason = "the host's vocabulary is three kinds; the tests name one"
    )]
    Api,
}

impl Host for TestHost {
    type Wall = TestWall;
    type Stall = Instant;

    fn wall_wire(kind: TestWall) -> &'static str {
        match kind {
            TestWall::Usage => "wall:usage",
            TestWall::Context => "wall:context",
            TestWall::Api => "wall:api-error",
        }
    }

    fn wall_band(kind: TestWall) -> &'static str {
        match kind {
            TestWall::Usage => "limited",
            TestWall::Context => "context full",
            TestWall::Api => "can't reach the API",
        }
    }

    fn is_aterms_holder(h: &str, pid: Option<u32>) -> bool {
        h == "harness@1" && pid == Some(1)
    }

    fn pct_encode(s: &str) -> String {
        s.replace(' ', "%20")
    }

    fn pct_decode(s: &str) -> String {
        s.replace("%20", " ")
    }

    fn columns(s: &str) -> usize {
        MEASURED.with(|m| m.set(m.get() + s.chars().count()));
        aterm_grapheme::str_grid_columns(s)
    }
}

thread_local! {
    static MEASURED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn measured(f: impl FnOnce()) -> usize {
    MEASURED.with(|m| m.set(0));
    f();
    MEASURED.with(std::cell::Cell::get)
}

type TSlot = Slot<TestHost>;
type TFacts = Facts<TestHost>;

fn agent(phase: AgentPhase<TestHost>) -> Option<AgentReading<TestHost>> {
    Some(AgentReading {
        phase,
        context_pct: Some(41),
    })
}

fn slot_of(facts: TFacts, now: Instant) -> TSlot {
    let mut s = TSlot::new(now);
    s.absorb(facts, now);
    s
}

fn driven_facts() -> TFacts {
    TFacts {
        hand: Hand::DrivenTurn {
            id: 41,
            holder: None,
        },
        ..TFacts::default()
    }
}

/// A published need of `severity`, `cause` and `key`.
pub(crate) fn need(severity: NeedSeverity, cause: StopCause, key: u64) -> NeedFact {
    NeedFact {
        severity,
        cause,
        key: (0, key),
        what: "never started".to_string(),
        show_pill: true,
    }
}

#[test]
fn the_wire_level_ranks_as_before_and_the_border_reads_no_level() {
    let now = Instant::now();
    let at = |f: TFacts| slot_of(f, now).level(0);
    assert_eq!(at(TFacts::default()), Level::Quiet);
    assert_eq!(
        at(TFacts {
            link: Link::Disconnected,
            ..TFacts::default()
        }),
        Level::Note
    );
    assert_eq!(
        at(TFacts {
            hand: Hand::Driving { sid: "s-1".into() },
            ..TFacts::default()
        }),
        Level::Driving
    );
    assert_eq!(at(driven_facts()), Level::Driven);
    assert_eq!(
        at(TFacts {
            agent_seq: 1,
            agent: agent(AgentPhase::Question),
            ..driven_facts()
        }),
        Level::Attention
    );
    let wall = AgentPhase::Wall {
        kind: TestWall::Usage,
        reset: None,
        until: None,
    };
    assert_eq!(
        at(TFacts {
            agent_seq: 1,
            agent: agent(wall),
            ..driven_facts()
        }),
        Level::Limited
    );
    assert_eq!(
        at(TFacts {
            input_stall: Some(now),
            ..TFacts::default()
        }),
        Level::Limited
    );
    assert_eq!(
        at(TFacts {
            hold: Some(HoldFact {
                reason: "pause".into(),
                fleet: false,
            }),
            ..driven_facts()
        }),
        Level::Hold
    );
    assert!(Level::Hold > Level::Limited && Level::Limited > Level::Attention);
    assert!(Level::Attention > Level::Driven && Level::Driven > Level::Driving);
    assert!(Level::Driving > Level::Story && Level::Story > Level::Note);
    let own = slot_of(
        TFacts {
            hand: Hand::DrivenLease {
                holder: "harness@1".into(),
                pid: Some(1),
            },
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(own.level(0), Level::Driven);
    let busy = slot_of(
        TFacts {
            agent_seq: 1,
            agent: agent(AgentPhase::Prompt { detail: None }),
            input_stall: Some(now),
            hold: Some(HoldFact {
                reason: "pause".into(),
                fleet: false,
            }),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(busy.level(0), Level::Hold);
    assert_eq!(busy.chip(), ChipLevel::Off);
    assert_eq!(busy.severity(), None);
}

#[test]
fn a_story_is_news_until_read_and_aterms_own_turn_is_none() {
    let now = Instant::now();
    let held = TFacts {
        hold: Some(HoldFact {
            reason: "pause".into(),
            fleet: false,
        }),
        ..TFacts::default()
    };
    let mut s = slot_of(TFacts::default(), now);
    s.absorb(held, now + Duration::from_secs(1));
    assert!(!s.calm(), "held");
    assert_eq!(s.level(0), Level::Hold);
    s.absorb(TFacts::default(), now + Duration::from_secs(61));
    assert!(s.calm());
    assert_eq!(s.level(0), Level::Story);
    assert_eq!(s.story_seq, 2, "the hold and its resume");
    assert_eq!(s.level(s.story_seq), Level::Quiet, "read");

    let mut own = slot_of(
        TFacts {
            hand: Hand::DrivenLease {
                holder: "harness@1".into(),
                pid: Some(1),
            },
            ..TFacts::default()
        },
        now,
    );
    own.absorb(
        TFacts {
            hand: Hand::DrivenLease {
                holder: "harness@1".into(),
                pid: Some(1),
            },
            turn: Some(TurnFact {
                id: 1,
                settled: true,
                dur_ms: 10,
                carried: false,
                aterm: false,
                opened: None,
            }),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(own.story_seq, 1, "counted");
    assert_eq!(own.tell(), 2, "a point told under the hand");
    own.absorb(TFacts::default(), now);
    assert_eq!(
        own.level(0),
        Level::Quiet,
        "not news, once the hand is let go: aterm's own turn and its told point"
    );
}

#[test]
fn a_programs_own_turn_end_is_a_story_point_and_counts_as_news() {
    let now = Instant::now();
    let at = |n: u64| TFacts {
        program_turn_ends: n,
        program_turn_starts: n,
        ..TFacts::default()
    };
    let mut s = slot_of(at(3), now);
    assert_eq!(s.story_seq, 0, "the count at the mint is the baseline");
    assert_eq!(s.turn_end_news, 0, "and no news");
    assert!(s.absorb(at(4), now + Duration::from_secs(1)));
    assert_eq!(s.story_seq, 1);
    assert_eq!(s.turn_end_news, 1, "news: nobody watched it");
    assert_eq!(s.level(0), Level::Story);
    assert_eq!(s.level(s.story_seq), Level::Quiet, "read");
    assert_eq!(
        s.chip(),
        ChipLevel::Off,
        "the dot is the host's, not the slot's"
    );
    assert!(!s.absorb(at(4), now + Duration::from_secs(3)));
    assert_eq!((s.story_seq, s.turn_end_news), (1, 1));
    let turn = TurnFact {
        id: 7,
        settled: true,
        dur_ms: 10,
        carried: false,
        aterm: false,
        opened: None,
    };
    s.absorb(
        TFacts {
            turn: Some(turn),
            ..at(5)
        },
        now + Duration::from_secs(4),
    );
    assert_eq!(s.story_seq, 2, "told once, not twice");
    assert_eq!(s.turn_end_news, 2, "and counted once");
    s.absorb(
        TFacts {
            program_turn_watched: true,
            ..at(6)
        },
        now + Duration::from_secs(5),
    );
    assert_eq!(s.story_seq, 3, "told");
    assert_eq!(s.turn_end_news, 2, "watched: no news");
}

#[test]
fn one_driven_turn_is_told_once_whichever_sweep_reads_which_end() {
    let now = Instant::now();
    let t = |s: u64| now + Duration::from_secs(s);
    let ledger = |id: u64, settled: bool| TurnFact {
        id,
        settled,
        dur_ms: 5_000,
        carried: false,
        aterm: false,
        opened: None,
    };
    let at = |starts: u64, ends: u64, turn: Option<TurnFact>| TFacts {
        program_turn_starts: starts,
        program_turn_ends: ends,
        turn,
        ..TFacts::default()
    };
    let mut s = slot_of(at(1, 0, None), now);
    assert!(s.absorb(at(1, 0, Some(ledger(1, true))), t(1)));
    assert!(s.absorb(at(1, 1, Some(ledger(1, true))), t(2)));
    assert_eq!(
        (s.story_seq, s.level(0)),
        (1, Level::Story),
        "settle, then done"
    );

    let mut s = slot_of(at(1, 0, None), now);
    assert!(s.absorb(at(1, 1, None), t(1)));
    assert!(s.absorb(at(1, 1, Some(ledger(1, true))), t(2)));
    assert_eq!(
        (s.story_seq, s.level(0)),
        (1, Level::Story),
        "done, then settle"
    );

    let mut s = slot_of(at(1, 0, None), now);
    assert!(s.absorb(at(1, 1, Some(ledger(1, true))), t(1)));
    assert_eq!(s.story_seq, 1, "one refresh");

    let mut s = slot_of(at(1, 0, None), now);
    s.absorb(at(1, 0, Some(ledger(1, true))), t(1));
    s.absorb(at(2, 1, Some(ledger(1, true))), t(2));
    assert_eq!((s.story_seq, s.level(0)), (2, Level::Story));

    let mut s = slot_of(at(1, 0, None), now);
    s.absorb(at(1, 0, Some(ledger(1, false))), t(1));
    s.absorb(at(1, 1, Some(ledger(1, false))), t(2));
    assert_eq!(s.story_seq, 2, "the timeout and the program's end");
    let mut s = slot_of(at(1, 0, None), now);
    s.absorb(at(1, 1, None), t(1));
    s.absorb(at(1, 1, Some(ledger(1, false))), t(2));
    assert_eq!(s.story_seq, 2, "the program's end and the timeout");

    let mut s = slot_of(at(1, 0, None), now);
    s.absorb(at(1, 0, Some(ledger(1, true))), t(1));
    s.absorb(at(1, 1, Some(ledger(1, true))), t(2));
    s.absorb(at(1, 1, Some(ledger(2, true))), t(3));
    assert_eq!(s.story_seq, 2, "the second settle has no end left to pair");

    let mut s = slot_of(at(1, 0, None), now);
    s.absorb(at(1, 1, None), t(1));
    s.absorb(at(1, 1, Some(ledger(1, true))), t(30));
    assert_eq!(
        (s.story_seq, s.level(0)),
        (2, Level::Story),
        "an end before the ledger turn opened"
    );
}

#[test]
fn a_turn_end_pairs_by_when_each_turn_began_in_every_order() {
    let now = Instant::now();
    let t = |s: u64| now + Duration::from_secs(s);
    let ledger = |opened: u64| TurnFact {
        id: 1,
        settled: true,
        dur_ms: 1_000,
        carried: false,
        aterm: false,
        opened: Some(t(opened)),
    };
    let at = |starts: u64, ends: u64, began: Option<u64>, turn: Option<TurnFact>| TFacts {
        program_turn_starts: starts,
        program_turn_ends: ends,
        program_turn_began: began.map(t),
        turn,
        ..TFacts::default()
    };
    let base = || slot_of(at(0, 0, None, None), now);
    for (order, reads) in [
        (
            "end, then settle",
            vec![
                (at(1, 1, Some(1), None), 7),
                (at(1, 1, Some(1), Some(ledger(5))), 9),
            ],
        ),
        (
            "settle, then end",
            vec![
                (at(1, 0, Some(1), Some(ledger(5))), 7),
                (at(1, 1, Some(1), Some(ledger(5))), 9),
            ],
        ),
        ("one refresh", vec![(at(1, 1, Some(1), Some(ledger(5))), 9)]),
    ] {
        let mut s = base();
        for (facts, when) in reads {
            s.absorb(facts, t(when));
        }
        assert_eq!(
            s.story_seq, 2,
            "the person's turn and the driven one: {order}"
        );
    }
    let mut s = base();
    s.absorb(at(1, 0, Some(1), None), t(2));
    s.absorb(at(1, 0, Some(1), Some(ledger(3))), t(6));
    s.absorb(at(1, 1, Some(1), Some(ledger(3))), t(8));
    assert_eq!(s.story_seq, 2, "the person's later end is its own turn");
    let mut s = base();
    s.absorb(at(1, 1, Some(6), None), t(8));
    s.absorb(at(1, 1, Some(6), Some(ledger(5))), t(38));
    assert_eq!(s.story_seq, 1, "a late read of the same turn's settle");
    for (order, reads) in [
        (
            "end, then settle",
            vec![
                (at(1, 1, Some(6), None), 7),
                (at(1, 1, Some(6), Some(ledger(5))), 9),
            ],
        ),
        (
            "settle, then end",
            vec![
                (at(1, 0, Some(6), Some(ledger(5))), 7),
                (at(1, 1, Some(6), Some(ledger(5))), 9),
            ],
        ),
        ("one refresh", vec![(at(1, 1, Some(6), Some(ledger(5))), 9)]),
    ] {
        let mut s = base();
        for (facts, when) in reads {
            s.absorb(facts, t(when));
        }
        assert_eq!(s.story_seq, 1, "one driven turn: {order}");
    }
}

#[test]
fn a_paired_owned_turn_does_not_leave_news_without_a_story() {
    let now = Instant::now();
    let t = |s: u64| now + Duration::from_secs(s);
    let ledger = Some(TurnFact {
        id: 1,
        settled: true,
        dur_ms: 5_000,
        carried: false,
        aterm: false,
        opened: None,
    });
    let at = |ends: u64, turn: Option<TurnFact>, owned: bool, watched: bool| TFacts {
        program_turn_starts: 1,
        program_turn_ends: ends,
        program_turn_watched: watched,
        turn,
        hand: if owned {
            Hand::DrivenLease {
                holder: "harness@1".into(),
                pid: Some(1),
            }
        } else {
            Hand::None
        },
        ..TFacts::default()
    };

    for done_first in [false, true] {
        let mut s = slot_of(at(0, None, false, false), now);
        s.absorb(
            at(
                u64::from(done_first),
                (!done_first).then_some(ledger).flatten(),
                false,
                false,
            ),
            t(1),
        );
        s.absorb(at(1, ledger, false, false), t(2));
        assert_eq!((s.story_seq, s.level(0)), (1, Level::Story));
    }
    let mut watched = slot_of(at(0, None, false, true), now);
    watched.absorb(at(1, None, false, true), t(1));
    assert_eq!(watched.level(0), Level::Quiet);
    watched.absorb(at(1, ledger, false, true), t(2));
    assert_eq!((watched.story_seq, watched.level(0)), (1, Level::Story));

    let owned = |done_first: bool, unrelated_mail: bool| {
        let mut s = slot_of(at(0, None, true, false), now);
        let first_turn = if done_first { None } else { ledger };
        let first_ends = u64::from(done_first);
        s.absorb(at(first_ends, first_turn, true, false), t(1));
        assert_eq!(s.story_seq, 1, "the own-hand point is counted");
        s.absorb(at(first_ends, first_turn, false, false), t(2));
        assert_eq!(s.level(0), Level::Quiet, "release alone is no news");
        let mut last = at(1, ledger, false, false);
        if unrelated_mail {
            last.mail.head = 1;
        }
        s.absorb(last, t(3));
        s
    };
    let mail = owned(false, true);
    assert_eq!((mail.story_seq, mail.level(0)), (2, Level::Story));

    let observed: Vec<_> = [false, true]
        .into_iter()
        .map(|done_first| {
            let s = owned(done_first, false);
            (s.story_seq, s.level(0))
        })
        .collect();
    assert_eq!(
        observed,
        vec![(1, Level::Quiet); 2],
        "PAIRED_OWNED_POINT_BECAME_NEWS_WITHOUT_A_STORY"
    );
}

#[test]
fn one_driven_turn_is_news_whichever_end_is_read_first() {
    let now = Instant::now();
    let t = |s: u64| now + Duration::from_secs(s);
    let ledger = Some(TurnFact {
        id: 1,
        settled: true,
        dur_ms: 5_000,
        carried: false,
        aterm: false,
        opened: None,
    });
    let at = |ends: u64, turn: Option<TurnFact>| TFacts {
        program_turn_starts: 1,
        program_turn_ends: ends,
        program_turn_watched: true,
        turn,
        ..TFacts::default()
    };
    let mut done_first = slot_of(at(0, None), now);
    done_first.absorb(at(1, None), t(1));
    assert_eq!(done_first.level(0), Level::Quiet, "control: the end alone");
    done_first.absorb(at(1, ledger), t(2));
    let mut settle_first = slot_of(at(0, None), now);
    settle_first.absorb(at(0, ledger), t(1));
    settle_first.absorb(at(1, ledger), t(2));
    for (order, s) in [("done first", &done_first), ("settle first", &settle_first)] {
        assert_eq!(s.story_seq, 1, "{order}: one point");
        assert_eq!(s.level(0), Level::Story, "{order}: news");
    }
}

#[test]
fn a_programs_turn_end_in_plain_sight_is_story_but_not_news() {
    let now = Instant::now();
    let at = |n: u64, watched: bool| TFacts {
        program_turn_ends: n,
        program_turn_watched: watched,
        ..TFacts::default()
    };
    let mut s = slot_of(at(0, true), now);
    assert!(s.absorb(at(1, true), now + Duration::from_secs(1)));
    assert_eq!(s.story_seq, 1, "the turn is in the story");
    assert_eq!(s.level(0), Level::Quiet, "but not news");
    assert!(s.absorb(at(2, false), now + Duration::from_secs(2)));
    assert_eq!(s.level(0), Level::Story, "unwatched, it is news");
}

#[test]
fn an_agents_story_closes_when_the_agent_leaves() {
    let now = Instant::now();
    let reading = |phase| AgentReading {
        phase,
        context_pct: None,
    };
    let mut s = TSlot::new(now);
    s.absorb(TFacts::default(), now);
    for (seq, phase) in [(1, AgentPhase::Question), (2, AgentPhase::Idle)] {
        s.absorb(
            TFacts {
                agent_seq: seq,
                agent: Some(reading(phase)),
                ..TFacts::default()
            },
            now,
        );
    }
    assert_eq!(s.level(0), Level::Story);
    s.absorb(
        TFacts {
            agent_seq: 3,
            agent: None,
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(s.level(0), Level::Quiet, "the agent left: its story closed");
    assert_eq!(
        s.story_seq, 1,
        "the closed point is still counted (`story=`)"
    );
    s.absorb(
        TFacts {
            turn: Some(TurnFact {
                id: 1,
                settled: true,
                dur_ms: 5,
                carried: false,
                aterm: false,
                opened: None,
            }),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(s.level(0), Level::Story, "a point after the exit is news");
}

#[test]
fn a_wall_the_harness_answers_is_a_note_and_a_wall_nobody_answers_is_limited() {
    let now = Instant::now();
    let wall_facts = |answer: WallAnswer| TFacts {
        agent_seq: 1,
        agent: agent(AgentPhase::Wall {
            kind: TestWall::Usage,
            reset: Some("4pm".to_string()),
            until: None,
        }),
        wall_answer: answer,
        ..TFacts::default()
    };
    let handled = slot_of(wall_facts(WallAnswer::Harness), now);
    let looked = handled.story_seq;
    assert_eq!(handled.level(0), Level::Story, "the wall's arrival is news");
    assert_eq!(handled.level(looked), Level::Note);
    assert!(!handled.calm());
    let asks = slot_of(wall_facts(WallAnswer::HarnessAsks("/model")), now);
    assert_eq!(asks.level(looked), Level::Note);
    let unanswered = slot_of(wall_facts(WallAnswer::Nobody), now);
    assert_eq!(unanswered.level(looked), Level::Limited);
    let prompt = slot_of(
        TFacts {
            agent_seq: 1,
            agent: agent(AgentPhase::Prompt { detail: None }),
            wall_answer: WallAnswer::Harness,
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(prompt.level(looked), Level::Attention);
    let mut s = slot_of(wall_facts(WallAnswer::Harness), now);
    assert!(
        s.absorb(wall_facts(WallAnswer::Nobody), now),
        "the answer moved"
    );
    let mut same = wall_facts(WallAnswer::Nobody);
    same.agent_seq = 2;
    assert!(!s.absorb(same, now), "nothing moved");
}

#[test]
fn the_chip_reads_the_published_need_and_the_persons_hold() {
    let now = Instant::now();
    let mark = |need: Option<NeedFact>, person: bool| {
        slot_of(
            TFacts {
                need,
                held_by_person: person,
                ..TFacts::default()
            },
            now,
        )
        .chip()
    };
    assert_eq!(mark(None, false), ChipLevel::Off);
    assert_eq!(mark(None, true), ChipLevel::Yours);
    assert_eq!(
        mark(Some(need(NeedSeverity::Decide, StopCause::Wall, 1)), false),
        ChipLevel::Wait
    );
    for cause in [StopCause::Wall, StopCause::Frozen, StopCause::Suspended] {
        assert_eq!(
            mark(Some(need(NeedSeverity::Stuck, cause, 1)), true),
            ChipLevel::Stop(cause),
            "a need outranks the hand"
        );
    }
    assert_eq!(chip_of_attention(true), ChipLevel::Note);
    assert_eq!(chip_of_attention(false), ChipLevel::Off);
    assert!(ChipLevel::Off < ChipLevel::Note && ChipLevel::Note < ChipLevel::Done);
    assert!(ChipLevel::Done < ChipLevel::Yours);
    assert!(ChipLevel::Yours < ChipLevel::Wait);
    assert!(ChipLevel::Wait < ChipLevel::Stop(StopCause::Wall));
    assert_eq!(ChipLevel::Note.chrome_states(), ["note"]);
    assert_eq!(ChipLevel::Done.chrome_states(), ["done"]);
    assert_eq!(ChipLevel::Yours.chrome_states(), ["yours"]);
    assert_eq!(
        ChipLevel::Stop(StopCause::Frozen).chrome_states(),
        ["attention", "stop"]
    );
}

#[test]
fn a_need_keeps_its_age_until_its_episode_moves() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let facts = |need: Option<NeedFact>| TFacts {
        need,
        ..TFacts::default()
    };
    let mut s = slot_of(
        facts(Some(need(NeedSeverity::Stuck, StopCause::Wall, 7))),
        t0,
    );
    assert_eq!(s.need_since, t0);
    let mut reworded = need(NeedSeverity::Stuck, StopCause::Wall, 7);
    reworded.what = "other words".to_string();
    assert!(s.absorb(facts(Some(reworded)), at(30)), "the words moved");
    assert_eq!(s.need_since, t0, "the same episode keeps its age");
    assert!(!s.absorb(
        facts(Some({
            let mut n = need(NeedSeverity::Stuck, StopCause::Wall, 7);
            n.what = "other words".to_string();
            n
        })),
        at(40)
    ));
    assert!(s.absorb(
        facts(Some(need(NeedSeverity::Stuck, StopCause::Wall, 8))),
        at(50)
    ));
    assert_eq!(s.need_since, at(50), "a new episode starts its own age");
    assert!(s.absorb(facts(None), at(60)));
    assert_eq!(s.chip(), ChipLevel::Off);
}

fn tones() -> PresenceTones {
    PresenceTones {
        wait: [1, 1, 1],
        stop: [2, 2, 2],
        yours: [3, 3, 3],
    }
}

#[test]
fn the_border_is_steady_and_a_quiet_view_costs_nothing() {
    let mut v = View::default();
    assert_eq!(v.fp(), 0);
    assert!(
        v.rim_glow(|| unreachable!("no tones on a quiet frame"))
            .is_none()
    );
    assert!(!v.show(Rim::None, None, None), "nothing moved");
    assert_eq!(v.fp(), 0);
    let mut glows = Vec::new();
    for rim in [Rim::Stuck, Rim::Decide, Rim::Yours] {
        assert!(v.show(rim, Some(1), None));
        let fp = v.fp();
        assert_ne!(fp, 0);
        assert!(!v.show(rim, Some(1), None));
        assert_eq!(v.fp(), fp, "unmoved");
        glows.push(v.rim_glow(tones).expect("lit"));
    }
    assert!(glows.iter().all(|g| g.wash_a == 0), "no wash");
    assert_eq!(glows[0].accent, tones().stop);
    assert_eq!(glows[1].accent, tones().wait);
    assert_eq!(glows[2].accent, tones().yours);
    assert_eq!(
        (glows[0].border_scale_q4, glows[1].border_scale_q4),
        (16, 16)
    );
    assert_eq!(
        glows[2].border_scale_q4,
        crate::presence::YOURS_BORDER_SCALE_Q4
    );
    assert!(glows[2].border_scale_q4 < 16, "the outline is thinner");
    assert_eq!(Rim::Stuck.wire(), "stuck");
    assert_eq!(Rim::of(NeedSeverity::Decide), Rim::Decide);
}

fn pill(kind: PillKind, tab: Option<usize>, act: Option<PillAct>, more: usize) -> Pill {
    Pill {
        kind,
        session: 3,
        label: if tab.is_some() { "fmt" } else { "" }.to_string(),
        tab,
        what: "never started".to_string(),
        since: tab.map(|_| Instant::now()),
        act,
        more,
    }
}

#[test]
fn the_pill_names_a_background_tab_and_offers_the_act_in_front() {
    let now = Instant::now();
    let stuck = PillKind::Need(NeedSeverity::Stuck);
    let back = pill_words(&pill(stuck, Some(4), None, 0), now);
    assert_eq!(back.glyph, '\u{25c6}');
    assert!(back.label.contains("fmt") && back.label.contains('4'));
    assert_eq!(back.act, None);
    assert_eq!(back.more, None);
    assert_eq!(
        back.text().split(" \u{b7} ").count(),
        3,
        "label, words, age"
    );
    let untitled = pill_words(
        &Pill {
            label: "tab 4".to_string(),
            ..pill(stuck, Some(4), None, 0)
        },
        now,
    );
    assert_eq!(untitled.label, "tab 4");
    assert_ne!(back.room, back.words);
    assert!(back.room.ends_with("59s"), "{:?}", back.room);
    let front = pill_words(&pill(stuck, None, Some(PillAct::Restart), 2), now);
    assert!(front.label.is_empty(), "the front tab is named already");
    assert_eq!(front.room, front.words, "no age, nothing to make room for");
    let pane = pill_words(
        &Pill {
            label: "fmt".to_string(),
            ..pill(stuck, None, None, 0)
        },
        now,
    );
    assert_eq!(pane.label, "fmt");
    assert_eq!(pane.room, pane.words);
    assert_eq!(front.act, Some(PillAct::Restart));
    assert_eq!(front.more.as_deref(), Some("+2"));
    let decide = pill_words(
        &pill(PillKind::Need(NeedSeverity::Decide), None, None, 0),
        now,
    );
    assert_eq!(decide.glyph, '\u{25c7}');
    let yours = pill_words(
        &pill(PillKind::Yours, None, Some(PillAct::HandBack), 0),
        now,
    );
    assert_eq!(yours.glyph, '\u{270b}');
    assert_eq!(yours.act, Some(PillAct::HandBack));
}

#[test]
fn pill_hit_regions_resolve_to_their_acts_through_the_layout() {
    let now = Instant::now();
    let words = pill_words(
        &pill(
            PillKind::Need(NeedSeverity::Stuck),
            None,
            Some(PillAct::Restart),
            2,
        ),
        now,
    );
    let cells = |s: &str| aterm_grapheme::str_grid_columns(s) as f64;
    let lay = pill_layout(&words, 200.0, 1.0, cells);
    let mut seen = Vec::new();
    let mut x = 0.0;
    while x < lay.width {
        if let Some(hit) = lay.hit(x)
            && seen.last() != Some(&hit)
        {
            seen.push(hit);
        }
        x += 0.5;
    }
    assert_eq!(
        seen,
        [PillHit::Go, PillHit::Act(PillAct::Restart), PillHit::More]
    );
    assert_eq!(lay.hit(-1.0), None);
    assert_eq!(lay.hit(lay.width + 1.0), None);
    let (_, act) = lay.act.expect("the act is drawn");
    assert_eq!(
        lay.hit(act.x + act.w / 2.0),
        Some(PillHit::Act(PillAct::Restart))
    );
}

#[test]
fn the_words_clip_first_then_the_label_never_the_glyph() {
    let now = Instant::now();
    let words = pill_words(
        &pill(
            PillKind::Need(NeedSeverity::Decide),
            Some(2),
            Some(PillAct::Restart),
            1,
        ),
        now,
    );
    let cells = |s: &str| aterm_grapheme::str_grid_columns(s) as f64;
    let full = pill_layout(&words, 500.0, 1.0, cells);
    for max in (0..=full.width as usize).rev() {
        let lay = pill_layout(&words, max as f64, 1.0, cells);
        assert_eq!(lay.glyph.w, full.glyph.w, "the glyph at {max}");
        assert!(lay.width <= (max as f64).max(full.glyph.w), "fits at {max}");
        if lay.label.w < full.label.w {
            assert_eq!(lay.words.w, 0.0, "the words went first at {max}");
        }
        if lay.act.is_none() {
            assert!(lay.more.is_none(), "the count goes before the act");
        }
    }
}

#[test]
fn sanitize_refuses_presence_glyphs_bidi_and_control_characters() {
    let s = sanitize_token::<TestHost>(
        "\u{2298} hold pause \u{00b7}fleet\u{1f512}  \u{25c2} evil\u{202e}gnp\u{7}\t\nname\u{2066}",
        64,
    );
    assert_eq!(s, "hold pause fleet evilgnp name");
    assert_eq!(
        sanitize_token::<TestHost>(&"x".repeat(200), 8),
        "xxxxxxx\u{2026}"
    );
    for c in [
        '\u{200e}', '\u{200f}', '\u{202a}', '\u{2069}', '\u{1b}', '~', '\u{2192}', '\u{25c6}',
        '\u{270b}',
    ] {
        assert_eq!(
            sanitize_token::<TestHost>(&format!("a{c}b"), 8),
            "ab",
            "{c:?}"
        );
    }
    assert_eq!(
        pill_text::<TestHost>("\u{25c6} ~/repo \u{b7} \u{270b}x\u{202e}", 32),
        "~/repo \u{b7} x"
    );
    assert_eq!(
        crate::presence::hold_reason_words::<TestHost>("main%20broken\u{202e}"),
        "main broken"
    );
    assert_eq!(
        Hand::DrivenLease {
            holder: "a b".into(),
            pid: None,
        }
        .wire::<TestHost>(),
        "lease:a%20b"
    );
}

#[test]
fn sanitize_strips_invisible_characters_on_every_path() {
    let hidden = [
        '\u{e0049}',
        '\u{e0047}',
        '\u{e004e}',
        '\u{e0001}',
        '\u{200b}',
        '\u{2060}',
        '\u{feff}',
        '\u{061c}',
        '\u{00ad}',
        '\u{180e}',
        '\u{200c}',
        '\u{200d}',
        '\u{3164}',
        '\u{034f}',
    ];
    for c in hidden {
        assert_eq!(
            sanitize_token::<TestHost>(&format!("a{c}b"), 8),
            "ab",
            "{c:?}"
        );
        assert_eq!(pill_text::<TestHost>(&format!("a{c}b"), 8), "ab", "{c:?}");
    }
    let smuggled = format!("ok{}end", hidden.iter().collect::<String>());
    assert_eq!(sanitize_token::<TestHost>(&smuggled, 64), "okend");
    assert_eq!(
        crate::presence::hold_reason_words::<TestHost>(
            "main%20broken\u{e0049}\u{e0047}\u{200b}\u{feff}"
        ),
        "main broken"
    );
}

#[test]
fn a_cell_holds_what_the_grids_cell_holds() {
    let marks = |n: usize| "\u{301}".repeat(n);
    let mut reset = String::from("1");
    for _ in 0..190 {
        reset.push('~');
        reset.push_str(&marks(16));
    }
    let one = sanitize_token::<TestHost>(&reset, 32);
    assert_eq!(one.chars().count(), 17, "one cell: `1` and 16 marks");
    assert_eq!(one, format!("1{}", marks(16)));
    assert_eq!(sanitize_token::<TestHost>("\u{301}lead", 8), "lead");
    assert_eq!(
        sanitize_token::<TestHost>("\u{2713}\u{301} \u{1f512}\u{301}x", 8),
        "x"
    );
    let many: String = (b'a'..=b'z')
        .chain(b'a'..=b'n')
        .map(|c| format!("{}{}", char::from(c), marks(40)))
        .collect();
    for cap in [1usize, 2, 8, 32] {
        for t in [
            sanitize_token::<TestHost>(&many, cap),
            pill_text::<TestHost>(&many, cap),
        ] {
            assert!(t.chars().count() <= 17 * cap, "cap {cap}: {t:?}");
            assert!(aterm_grapheme::str_grid_columns(&t) <= cap, "cap {cap}");
        }
    }
}

#[test]
fn the_cell_walk_is_linear_in_its_string() {
    for n in [256usize, 4096] {
        let raw = format!("a{}", "\u{301}".repeat(n));
        for (name, work) in [
            (
                "sanitize",
                measured(|| drop(sanitize_token::<TestHost>(&raw, 32))),
            ),
            ("pill", measured(|| drop(pill_text::<TestHost>(&raw, 32)))),
        ] {
            assert!(
                work <= 8 * (n + 1),
                "{name}: {work} characters measured for {}",
                n + 1
            );
        }
    }
}

#[test]
fn durations_and_the_pills_age_step_by_their_own_units() {
    assert_eq!(fmt_dur(Duration::from_secs(12)), "12s");
    assert_eq!(fmt_dur(Duration::from_secs(192)), "3m12s");
    assert_eq!(fmt_dur(Duration::from_secs(7_500)), "2h05m");
    assert_eq!(fmt_dur(Duration::from_secs(165_600)), "1d 22h");
    assert_eq!(short_sid("s-1e918c4662a1b7b8bd43"), "s-1e91");
    for (secs, age) in [(5, "5s"), (125, "2m"), (7_500, "2h"), (172_800, "2d")] {
        assert_eq!(pill_age(Duration::from_secs(secs)), age);
    }
    for secs in [
        0u64, 9, 10, 59, 60, 599, 3_599, 3_600, 86_399, 86_400, 950_400,
    ] {
        let d = Duration::from_secs(secs);
        let (age, room) = (pill_age(d), pill_age_room(d));
        assert!(age.len() <= room.len(), "{secs}: {age} in {room}");
        assert_eq!(age.chars().last(), room.chars().last(), "{secs}: one unit");
    }
    assert_eq!(
        pill_age_room(Duration::from_secs(3)),
        pill_age_room(Duration::from_secs(47))
    );
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    assert_eq!(pill_due(t0, t0), at(1));
    assert_eq!(pill_due(t0, at(59)), at(60));
    assert_eq!(pill_due(t0, at(61)), at(120));
    assert_eq!(pill_due(t0, at(3_601)), at(7_200));
    for s in [0u64, 30, 59, 60, 61, 3_599, 3_600, 90_000] {
        let now = at(s) + Duration::from_millis(400);
        let due = pill_due(t0, now);
        assert!(due > now, "after now at {s}");
        assert_ne!(
            pill_age(due.saturating_duration_since(t0)),
            pill_age(now.saturating_duration_since(t0)),
            "the figure moves at the due {s}"
        );
    }
}

#[test]
fn the_state_line_has_a_piece_per_fact_it_knows() {
    let now = Instant::now();
    let pieces = |s: &TSlot, program: Option<&str>| {
        state_line(s, program).map(|l| l.split(" \u{b7} ").count())
    };
    let busy = slot_of(
        TFacts {
            agent_seq: 1,
            agent: agent(AgentPhase::Busy),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(pieces(&busy, Some("claude")), Some(3));
    assert_eq!(pieces(&busy, None), Some(2));
    let shell = slot_of(
        TFacts {
            shell: Some(("running", now)),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(pieces(&shell, None), Some(1));
    let line = state_line(&shell, None).expect("a shell's word");
    assert!(!line.chars().any(|c| c.is_ascii_digit()), "no age: {line}");
    assert_eq!(pieces(&TSlot::new(now), None), None);
    let unknown = slot_of(
        TFacts {
            agent_seq: 1,
            agent: agent(AgentPhase::Unknown),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(pieces(&unknown, None), None);
}

#[test]
fn the_programs_own_report_is_named_only_where_the_reader_names_no_phase() {
    let now = Instant::now();
    let facts = |phase: Option<AgentPhase<TestHost>>| TFacts {
        agent_seq: 1,
        agent: phase.map(|phase| AgentReading {
            phase,
            context_pct: None,
        }),
        shell: Some(("running", now)),
        program: Some(("codex working", now)),
        ..TFacts::default()
    };
    for phase in [None, Some(AgentPhase::Unknown)] {
        let line = state_line(&slot_of(facts(phase), now), Some("codex"));
        assert_eq!(line.as_deref(), Some("codex working"));
    }
    let line = state_line(&slot_of(facts(Some(AgentPhase::Busy)), now), Some("codex"));
    assert_eq!(line.as_deref(), Some("codex \u{b7} busy"));
    let quiet = TFacts {
        program: None,
        ..facts(None)
    };
    assert_eq!(
        state_line(&slot_of(quiet, now), None).as_deref(),
        Some("running")
    );
    let mcp = slot_of(
        TFacts {
            agent_seq: 1,
            agent: agent(AgentPhase::Prompt {
                detail: Some("mcp-server".into()),
            }),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(
        state_line(&mcp, Some("claude")).as_deref(),
        Some("claude \u{b7} MCP server approval \u{b7} ctx 41% left")
    );
}

/// A PROGRAM THAT NAMES NO PHASE SAYS ITS CONTEXT BESIDE ITS OWN REPORT (codex's rollout figure):
/// the hover reads `codex working · ctx 40% left`, warned at the agent's threshold, as a Claude
/// tab's figure is, and a figure that moves alone redraws it. NEGATIVE CONTROLS: a report with no
/// figure says the report alone; a reader that names a phase says its own figure, never the
/// program's; no report says no figure.
#[test]
fn a_programs_own_context_figure_follows_its_report() {
    let now = Instant::now();
    let facts = |phase: Option<AgentPhase<TestHost>>, ctx: Option<u8>| TFacts {
        agent_seq: 1,
        agent: phase.map(|phase| AgentReading {
            phase,
            context_pct: None,
        }),
        shell: Some(("running", now)),
        program: Some(("codex working", now)),
        program_ctx_left: ctx,
        ..TFacts::default()
    };
    let line = |facts: TFacts| state_line(&slot_of(facts, now), Some("codex"));
    assert_eq!(
        line(facts(None, Some(40))).as_deref(),
        Some("codex working \u{b7} ctx 40% left")
    );
    assert_eq!(
        line(facts(Some(AgentPhase::Unknown), Some(15))).as_deref(),
        Some("codex working \u{b7} ctx 15% left \u{26a0}")
    );
    assert_eq!(line(facts(None, None)).as_deref(), Some("codex working"));
    assert_eq!(
        line(facts(Some(AgentPhase::Busy), Some(40))).as_deref(),
        Some("codex \u{b7} busy")
    );
    assert_eq!(
        line(TFacts {
            program: None,
            ..facts(None, Some(40))
        })
        .as_deref(),
        Some("codex \u{b7} running")
    );
    let mut slot = slot_of(facts(None, Some(40)), now);
    assert!(slot.absorb(facts(None, Some(30)), now), "the figure moved");
    assert!(!slot.absorb(facts(None, Some(30)), now), "nothing moved");
}

#[test]
fn an_unread_task_waits_only_with_no_hand_on_the_session() {
    let now = Instant::now();
    let mail = MailFacts {
        unread: 1,
        head: 1,
        last: Some(MailLast {
            kind: "task".into(),
            from: "manager".into(),
            trust: "agent".into(),
        }),
        ..MailFacts::default()
    };
    let free = slot_of(
        TFacts {
            mail: mail.clone(),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(free.level(0), Level::Attention);
    assert_eq!(free.why(0), "mail");
    let driven = slot_of(
        TFacts {
            mail,
            ..driven_facts()
        },
        now,
    );
    assert_eq!(driven.level(0), Level::Driven);
    assert_eq!(driven.chip(), ChipLevel::Off, "mail is no need");
}

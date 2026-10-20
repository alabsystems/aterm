// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The presence core without a host: a [`TestHost`] with a three-kind wall
//! and a plain stall drives the wire level ranking, the story, the need's
//! mark and age, the border's glow, the pill's words and geometry, the
//! sanitizer and the idle view.

use crate::presence::{
    AgentPhase, AgentReading, ChipLevel, Facts, Hand, HoldFact, Host, Level, Link, MailFacts,
    MailLast, NeedFact, NeedSeverity, Pill, PillAct, PillHit, PillKind, PresenceTones, Rim, Slot,
    StopCause, StoryVerb, TurnFact, View, WallAnswer, chip_of_attention, fmt_dur, pill_age,
    pill_age_room, pill_due, pill_layout, pill_text, pill_words, sanitize_token, short_sid,
    state_line,
};
use crate::{Duration, Instant};

/// A test host: its walls are a usage window, a full context and an API
/// error, its stall is the instant it began, its harness is `harness@1`, its
/// percent codec spells a space `%20`, and its grid is the terminal's own
/// (`aterm_grapheme::str_grid_columns`), counting the characters it measures
/// on this thread ([`MEASURED`]).
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
    /// The characters [`TestHost::columns`] has measured on this thread: the
    /// work a walk of a token's cells does, read without a clock.
    static MEASURED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The characters `f` has the host measure.
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
    }
}

/// `status level=` keeps its ranking, a record unchanged by the owner
/// surfaces: `hold > limited > attention > driven > driving > story > note >
/// quiet`, a stall ranks with a wall, and aterm's own hand still counts.
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
    // aterm's own hand is a hand on the wire.
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
    // NEGATIVE CONTROL for the border: a held, stalled, prompting slot with
    // no published need has no mark — the level is no need.
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

/// The story's model half: a hold that lifts leaves a story, which is not
/// calm while held and calm after; reading it (the watermark at the story's
/// seq) quiets `level=`; a turn under aterm's own hand is counted, never
/// news.
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
    assert_eq!(s.story_since(0).count(), 2, "the hold and its resume");
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
    assert_eq!(own.story_since(0).count(), 0, "not news");
    // A told choice is one point, `status story=`'s.
    assert_eq!(own.tell(StoryVerb::Chose, now), 2);
}

/// A background codex finishing its turn (OSC 7501 `working` → `done`, the
/// host's count of them) is a story point: `status story=` counts it and
/// `level=story` says it until it is read, and as news it counts in
/// `turn_end_news`, which the host's tab `•` reads (the slot's own mark stays
/// the need's and the hold's). The count standing at the slot's first read
/// is the baseline, not news, and a turn the ledger settled in the same
/// refresh is told once and counted once. NEGATIVE CONTROL: an end the
/// person watched (`program_turn_watched`) is a story point but no news, so
/// it counts nothing.
#[test]
fn a_programs_own_turn_end_is_a_story_point_and_counts_as_news() {
    let now = Instant::now();
    // Each end follows its own start.
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
    // No new end, no new point.
    assert!(!s.absorb(at(4), now + Duration::from_secs(3)));
    assert_eq!((s.story_seq, s.turn_end_news), (1, 1));
    // A ledger turn settling in the same refresh is the one point.
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
    // NEGATIVE CONTROL: an end in plain sight is told, never news.
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

/// One driven codex turn ends twice, the ledger's settle and the program's
/// own report, and the sweeps that read them land in either order: it is
/// told ONCE either way, and in the same refresh. NEGATIVE CONTROLS: a turn
/// of the program's own that began after the settle is a second turn; a
/// timed-out ledger turn pairs with nothing, so it and the program's end
/// are both told; an end pairs only once; a program end told before the
/// ledger turn opened (the person's own turn, then a driven one the program
/// never worked on) is an earlier turn's.
#[test]
fn one_driven_turn_is_told_once_whichever_sweep_reads_which_end() {
    let now = Instant::now();
    let t = |s: u64| now + Duration::from_secs(s);
    // Each ledger turn ran 5 s up to its settle.
    let ledger = |id: u64, settled: bool| TurnFact {
        id,
        settled,
        dur_ms: 5_000,
        carried: false,
        aterm: false,
        opened: None,
    };
    // `starts` turns begun and `ends` ended by the program; the ledger's last.
    let at = |starts: u64, ends: u64, turn: Option<TurnFact>| TFacts {
        program_turn_starts: starts,
        program_turn_ends: ends,
        turn,
        ..TFacts::default()
    };
    let turns = |s: &TSlot| s.story_since(0).count();

    // The settle first, the program's end a sweep later.
    let mut s = slot_of(at(1, 0, None), now);
    assert!(s.absorb(at(1, 0, Some(ledger(1, true))), t(1)));
    assert!(s.absorb(at(1, 1, Some(ledger(1, true))), t(2)));
    assert_eq!((s.story_seq, turns(&s)), (1, 1), "settle, then done");

    // The program's end first, the settle a sweep later.
    let mut s = slot_of(at(1, 0, None), now);
    assert!(s.absorb(at(1, 1, None), t(1)));
    assert!(s.absorb(at(1, 1, Some(ledger(1, true))), t(2)));
    assert_eq!((s.story_seq, turns(&s)), (1, 1), "done, then settle");

    // Both in one refresh.
    let mut s = slot_of(at(1, 0, None), now);
    assert!(s.absorb(at(1, 1, Some(ledger(1, true))), t(1)));
    assert_eq!(s.story_seq, 1, "one refresh");

    // Control: the program began a turn of its own after the settle; its end
    // is a second turn.
    let mut s = slot_of(at(1, 0, None), now);
    s.absorb(at(1, 0, Some(ledger(1, true))), t(1));
    s.absorb(at(2, 1, Some(ledger(1, true))), t(2));
    assert_eq!((s.story_seq, turns(&s)), (2, 2));

    // Control: a timed-out ledger turn pairs with nothing.
    let mut s = slot_of(at(1, 0, None), now);
    s.absorb(at(1, 0, Some(ledger(1, false))), t(1));
    s.absorb(at(1, 1, Some(ledger(1, false))), t(2));
    assert_eq!(s.story_seq, 2, "the timeout and the program's end");
    let mut s = slot_of(at(1, 0, None), now);
    s.absorb(at(1, 1, None), t(1));
    s.absorb(at(1, 1, Some(ledger(1, false))), t(2));
    assert_eq!(s.story_seq, 2, "the program's end and the timeout");

    // Control: an end pairs once. Settle (told), done (paired), then a
    // second settle with no turn begun between: a turn the program did no
    // work for, told on its own.
    let mut s = slot_of(at(1, 0, None), now);
    s.absorb(at(1, 0, Some(ledger(1, true))), t(1));
    s.absorb(at(1, 1, Some(ledger(1, true))), t(2));
    s.absorb(at(1, 1, Some(ledger(2, true))), t(3));
    assert_eq!(s.story_seq, 2, "the second settle has no end left to pair");

    // Control: the person's own codex turn ends (told, unpaired), then a
    // driven turn the program never works on opens after it and settles:
    // two turns, though no turn began between the reads.
    let mut s = slot_of(at(1, 0, None), now);
    s.absorb(at(1, 1, None), t(1));
    s.absorb(at(1, 1, Some(ledger(1, true))), t(30));
    assert_eq!(
        (s.story_seq, turns(&s)),
        (2, 2),
        "an end before the ledger turn opened"
    );
}

/// The pairing does not depend on the order of the reads or on whether one
/// refresh reads both, because it is decided by WHEN each turn began: the
/// program's turn (`program_turn_began`) no earlier than the ledger's opened
/// (`TurnFact::opened`, the ledger's own `started_ms`). The person's own codex
/// turn begins at t1; a driven turn the program never works on opens at t5
/// and settles: two turns, whichever refresh reads which end, the same refresh
/// included; and a driven turn typed while the person's turn still runs is
/// told apart from the person's later end. The settle read long after the
/// program's end of the same driven turn still pairs (the ledger's open, not
/// the read, dates it). NEGATIVE CONTROLS: the driven turn the program DID
/// work on (began at t6) is one turn in every order.
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
    // The program's counts and its latest start; the ledger's turn.
    let at = |starts: u64, ends: u64, began: Option<u64>, turn: Option<TurnFact>| TFacts {
        program_turn_starts: starts,
        program_turn_ends: ends,
        program_turn_began: began.map(t),
        turn,
        ..TFacts::default()
    };
    let base = || slot_of(at(0, 0, None, None), now);
    // (a) The person's turn (t1) ends; the driven turn (opened t5) settles.
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
    // (b) Typed while the person's turn runs: the settle is read at the
    // person's count, then the person's own end.
    let mut s = base();
    s.absorb(at(1, 0, Some(1), None), t(2));
    s.absorb(at(1, 0, Some(1), Some(ledger(3))), t(6));
    s.absorb(at(1, 1, Some(1), Some(ledger(3))), t(8));
    assert_eq!(s.story_seq, 2, "the person's later end is its own turn");
    // (c) The settle of the turn the program worked on, read 30 s after the
    // program's end (past its 1 s `dur_ms`): one turn.
    let mut s = base();
    s.absorb(at(1, 1, Some(6), None), t(8));
    s.absorb(at(1, 1, Some(6), Some(ledger(5))), t(38));
    assert_eq!(s.story_seq, 1, "a late read of the same turn's settle");
    // NEGATIVE CONTROL: the driven turn the program worked on is one turn in
    // every order.
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

/// A turn already recorded under aterm's own hand stays omitted when its
/// delayed counterpart arrives after that hand was released. A watched foreign
/// point is different: pairing may make that retained point news. Both orders
/// use the actual fold and story/level queries, with no live host.
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

    // A foreign turn is still one news point whichever end is read first.
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
        assert_eq!(
            (s.story_seq, s.story_since(0).count(), s.level(0)),
            (1, 1, Level::Story)
        );
    }
    // A watched, non-owned point may gain news from the settle that pairs it.
    let mut watched = slot_of(at(0, None, false, true), now);
    watched.absorb(at(1, None, false, true), t(1));
    assert_eq!(watched.level(0), Level::Quiet);
    watched.absorb(at(1, ledger, false, true), t(2));
    assert_eq!(
        (
            watched.story_seq,
            watched.story_since(0).count(),
            watched.level(0)
        ),
        (1, 1, Level::Story)
    );

    let owned = |done_first: bool, unrelated_mail: bool| {
        let mut s = slot_of(at(0, None, true, false), now);
        let first_turn = if done_first { None } else { ledger };
        let first_ends = u64::from(done_first);
        s.absorb(at(first_ends, first_turn, true, false), t(1));
        assert_eq!(s.story_since(0).count(), 0, "own-hand point stays omitted");
        // Release the hand without either counter moving, then read its other end.
        s.absorb(at(first_ends, first_turn, false, false), t(2));
        assert_eq!(s.level(0), Level::Quiet, "release alone is no news");
        let mut last = at(1, ledger, false, false);
        if unrelated_mail {
            last.mail.head = 1;
        }
        s.absorb(last, t(3));
        s
    };
    // Pairing does not erase an unrelated news point beside the owned turn.
    let mail = owned(false, true);
    assert_eq!(
        (mail.story_seq, mail.story_since(0).count(), mail.level(0)),
        (2, 1, Level::Story)
    );

    let observed: Vec<_> = [false, true]
        .into_iter()
        .map(|done_first| {
            let s = owned(done_first, false);
            (s.story_seq, s.story_since(0).count(), s.level(0))
        })
        .collect();
    assert_eq!(
        observed,
        vec![(1, 0, Level::Quiet); 2],
        "PAIRED_OWNED_POINT_BECAME_NEWS_WITHOUT_A_STORY"
    );
}

/// The level a driven turn leaves does not depend on which end is read
/// first: told by the program's end in plain sight (no news), the settle that
/// pairs with it is still the news a settle is, as it is read the other way
/// round. NEGATIVE CONTROL: the watched end alone stays no news.
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

/// A codex finishing its turn in the tab the person is looking at joins the
/// story without being news: `level=` stays quiet. NEGATIVE CONTROL: the
/// same end unwatched is news.
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

/// The story an agent told while nobody looked closes when the agent leaves;
/// a new point after the exit is news again.
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
    assert_eq!(s.story_since(0).count(), 0);
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

/// A wall the harness answers stands at `note` on the wire; one nobody
/// answers is `limited`; the answer moving is a change.
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
    // Control: a prompt and a peer's hand outrank the note.
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

/// THE MARK READS THE PUBLISHED NEED: a stuck need is the filled diamond with
/// its cause, a decide need the hollow one, the person's hold the hand where
/// nothing needs them, else none. A need outranks the hand, and the marks
/// rank `Off < Done < Yours < Wait < Stop` so a tab's max keeps the most
/// pressing.
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
    // The tab's own indicator is a note, ranked under every presence mark,
    // and never one of the need marks.
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

/// A need's age counts from its episode: the same key re-read keeps it (its
/// words may move), another key starts its own, and none clears it.
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

/// THE BORDER IS STEADY: stuck in the stop tone and decide in the wait tone
/// at the plain law's width and the drop target's alpha, the yours outline in
/// the dim ink at one pixel, and never a wash. A quiet view's term is exactly
/// 0 and reads no tones; a lit one is nonzero and moves only on a change.
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
        // The front session's pill names nothing; any other its label.
        label: if tab.is_some() { "fmt" } else { "" }.to_string(),
        tab,
        what: "never started".to_string(),
        // A background tab's pill alone carries its need's age.
        since: tab.map(|_| Instant::now()),
        act,
        more,
    }
}

/// THE PILL'S PIECES: a background tab's pill carries its label and number
/// and its age, a front tab's neither, the act only where offered, `+N` only
/// with more needs, the hand's pill its own glyph and Hand Back. Asserted as
/// pieces and kinds, never as sentences.
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
    // A tab nothing names but its number is named once.
    let untitled = pill_words(
        &Pill {
            label: "tab 4".to_string(),
            ..pill(stuck, Some(4), None, 0)
        },
        now,
    );
    assert_eq!(untitled.label, "tab 4");
    // The room the words are measured at: the age at its unit's widest.
    assert_ne!(back.room, back.words);
    assert!(back.room.ends_with("59s"), "{:?}", back.room);
    let front = pill_words(&pill(stuck, None, Some(PillAct::Restart), 2), now);
    assert!(front.label.is_empty(), "the front tab is named already");
    assert_eq!(front.room, front.words, "no age, nothing to make room for");
    // Another pane of the front tab: its label alone, no number, no age.
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

/// PILL HIT REGIONS RESOLVE TO THEIR ACTS THROUGH THE PAINTER'S OWN GEOMETRY:
/// every unit of the laid-out pill is the body, the act or the `+N`, in
/// that order left to right, and the margins resolve to nothing.
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

/// THE WORDS CLIP FIRST, THEN THE LABEL, NEVER THE GLYPH: narrowing the pill
/// shrinks the words to nothing before the label gives a unit, the glyph
/// keeps its width at every size, and the `+N` then the act go only where
/// the glyph alone would not fit beside them.
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

/// A pill's free text keeps a path's `~` and a sentence's `·` but never the
/// pill's own glyphs, and the wire token refuses presence's whole
/// vocabulary, bidi and controls; the cap cuts with `…`.
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

/// The token strips what the band strips (TAG characters, ZWSP, WJ, the BOM,
/// the Arabic letter mark, the soft hyphen, U+180E, the joiners, a Hangul
/// filler, CGJ) on both paths.
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

/// A cell holds what the grid's cell holds: `1` and its first 16 marks, a
/// mark with no cell to join is dropped, and a token is at most 17
/// characters a cell of its cap.
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

/// The walk of a token's cells is linear in what it is given, even on a raw
/// string of one cell with 4,096 marks (counted in what the host measured,
/// not a clock).
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

/// The durations: the wire's `fmt_dur`, the pill's coarse age, and the
/// pill's next figure, never before the next whole unit of what it prints.
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
    // The room an age is measured at: its unit's widest figure, as many
    // characters as any age in that unit and one figure for all of them.
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

/// The hover's state line: the program, the phase, the context left — three
/// pieces for an agent, one for a shell, none for nothing — and no age (the
/// hover is shown long after it is composed).
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
    // An unknown phase is no phase to name.
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

/// The program's own report (OSC 7501, `Facts::program`) is the state line's
/// word where no agent reading names a phase, ahead of the shell's, and it
/// names its program itself; a phase the screen's reader names keeps the line
/// (the report is display, never the verdict). A box reads in a person's
/// words, an MCP server's dialog too.
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
    // Control: a phase the reader names keeps the line.
    let line = state_line(&slot_of(facts(Some(AgentPhase::Busy)), now), Some("codex"));
    assert_eq!(line.as_deref(), Some("codex \u{b7} busy"));
    // Control: with no report the shell's word shows.
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

/// `ctl story` takes `chose` alone: the seven words no process sent are
/// usage errors now.
#[test]
fn only_chose_is_a_told_story_word() {
    assert_eq!(StoryVerb::TOLD_WORDS, ["chose"]);
    assert_eq!(StoryVerb::parse_told("chose"), Some(StoryVerb::Chose));
    for gone in [
        "approved",
        "dismissed",
        "reconnected",
        "timeout",
        "exit",
        "compacted",
        "warned",
    ] {
        assert_eq!(StoryVerb::parse_told(gone), None, "{gone}");
    }
}

/// Mail still weighs on the wire level: an unread task with no hand is
/// `attention`, with a hand it is the hand's.
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

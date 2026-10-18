// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The presence core without a host: a [`TestHost`] with a three-kind wall
//! and a plain stall drives the level ranking, the fold law, the width law,
//! the sanitizer, the painter, the idle view, and the row's words (elision,
//! severity, the story and its watermark, the tone, durations, the ripple).

use crate::ink::{BandInks, InkedCell};
use crate::presence::{
    AgentPhase, AgentReading, ChipLevel, Facts, Hand, HoldFact, Host, Level, Link, MailFacts,
    MailLast, PresenceTones, RIPPLE, Rim, Slot, SlotKind, StopCause, Tone, TurnFact, View,
    WallAnswer, Words, fmt_dur, paint_row, prints_seconds, prompt_band_word, sanitize_token,
    short_sid, stall_words, text_cols, words, words_step,
};
use crate::{Duration, Instant};

/// A test host: its walls are a usage window, a full context and a retried
/// API error, its stall is `(since, stopped, survived, restart)`, its harness
/// is `harness@1`, its percent codec spells a space `%20`, and its grid is
/// the terminal's own (`aterm_grapheme::str_grid_columns`), counting the
/// characters it measures on this thread ([`MEASURED`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TestHost {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TestWall {
    Usage,
    Context,
    /// An API error the harness retries on its own clock
    /// ([`Host::wall_retries`]).
    Api,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TestStall {
    since: Instant,
    stopped: bool,
    survived: bool,
    restart: Option<String>,
}

impl Host for TestHost {
    type Wall = TestWall;
    type Stall = TestStall;

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

    fn wall_retries(kind: TestWall) -> bool {
        kind == TestWall::Api
    }

    fn stall_since(s: &TestStall) -> Instant {
        s.since
    }

    fn stall_stopped(s: &TestStall) -> bool {
        s.stopped
    }

    fn stall_survived(s: &TestStall) -> bool {
        s.survived
    }

    fn stall_restart(s: &TestStall) -> Option<&str> {
        s.restart.as_deref()
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
    /// work a walk of the row's cells does, read without a clock.
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

/// The design's first row: role, phase, a peer's turn, a task, ctx, rtt.
fn driven(now: Instant) -> TSlot {
    slot_of(
        TFacts {
            role: Some("worker:claude-satcomp".into()),
            agent_seq: 1,
            agent: agent(AgentPhase::Busy),
            hand: Hand::DrivenTurn {
                id: 41,
                holder: Some("manager".into()),
            },
            mail: MailFacts {
                unread: 2,
                head: 2,
                last: Some(MailLast {
                    kind: "task".into(),
                    from: "manager".into(),
                    trust: "agent".into(),
                }),
                ..MailFacts::default()
            },
            link: Link::Connected { rtt_ms: Some(12) },
            ..TFacts::default()
        },
        now,
    )
}

/// `hold > limited > attention > driven > driving > story > note > quiet`,
/// and a stall ranks with a wall; aterm's own hand and an attention told
/// elsewhere count for `level` but not for what the window shows.
#[test]
fn levels_rank_as_the_design_table_and_the_window_shows_less() {
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
    let stall = TestStall {
        since: now,
        stopped: false,
        survived: false,
        restart: None,
    };
    assert_eq!(
        at(TFacts {
            input_stall: Some(stall),
            ..TFacts::default()
        }),
        Level::Limited
    );
    assert_eq!(
        at(TFacts {
            hold: Some(crate::presence::HoldFact {
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

    // aterm's own hand: `level` keeps it, the window does not show it.
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
    assert_eq!(own.shown_level(0), Level::Quiet);
    // Another instance's harness is somebody else's hand.
    let other = slot_of(
        TFacts {
            hand: Hand::DrivenLease {
                holder: "harness@2".into(),
                pid: None,
            },
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(other.shown_level(0), Level::Driven);
    // An attention told elsewhere: a fact, not the window's words.
    let told = slot_of(
        TFacts {
            attention: Some("upgrade stalled".into()),
            attention_told_elsewhere: true,
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(told.level(0), Level::Attention);
    assert_eq!(told.shown_level(0), Level::Quiet);
    assert_eq!(Level::Hold.rim(), Rim::Stop { hold: true });
    assert_eq!(Level::Driving.rim(), Rim::None);
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

/// The fold law's model half: a hold that lifts leaves a story, which is not
/// calm while held and calm after; reading it (the watermark at the story's
/// seq) folds the row; a turn under aterm's own hand is no story at all.
#[test]
fn the_fold_law_reads_a_story_only_when_calm() {
    let now = Instant::now();
    let held = TFacts {
        hold: Some(crate::presence::HoldFact {
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
    let w = words(&s, now + Duration::from_secs(62), 0);
    assert!(
        w.since.iter().any(|c| c == "held 1m00s, resumed 1s ago"),
        "{w:?}"
    );
    assert_eq!(s.level(s.story_seq), Level::Quiet, "read: the row folds");

    // A turn aterm's own harness typed is never news.
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
            turn: Some(crate::presence::TurnFact {
                id: 1,
                settled: true,
                dur_ms: 10,
                carried: false,
            }),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(own.story_seq, 1, "counted");
    assert_eq!(own.story_since(0).count(), 0, "not news");
    // A pending question is not calm; the host's wall word is the row's.
    let wall = slot_of(
        TFacts {
            agent_seq: 1,
            agent: agent(AgentPhase::Wall {
                kind: TestWall::Context,
                reset: None,
                until: None,
            }),
            ..TFacts::default()
        },
        now,
    );
    assert!(!wall.calm());
    assert_eq!(words(&wall, now, 0).phase, "context full");
}

/// No jargon on the row: a phase the reader could not name prints no
/// `unknown` and no age, and a session with no mail prints no `✉0` (the slot
/// is gone). Controls: a named phase prints the phase and its age, one unread
/// mail prints `✉1`, and a pending or queued count alone still shows the slot.
#[test]
fn the_row_prints_no_unknown_phase_and_no_empty_mail_slot() {
    let now = Instant::now();
    let facts = |phase: AgentPhase<TestHost>, mail: MailFacts| TFacts {
        agent_seq: 1,
        agent: Some(AgentReading {
            phase,
            context_pct: None,
        }),
        hand: Hand::DrivenTurn {
            id: 7,
            holder: Some("r34-driver".into()),
        },
        mail,
        ..TFacts::default()
    };
    let later = now + Duration::from_secs(215);
    let w = words(
        &slot_of(facts(AgentPhase::Unknown, MailFacts::default()), now),
        later,
        0,
    );
    let row = w.fit::<TestHost>(120);
    assert!(!row.contains("unknown"), "{row}");
    assert!(
        !row.contains("3m35s"),
        "no age for a phase nobody named: {row}"
    );
    assert!(!row.contains('\u{2709}'), "no mail, no slot: {row}");
    assert!(w.mail.is_empty() && w.mail_short.is_empty());
    assert!(row.contains("\u{25c2} r34-driver"), "the hand stays: {row}");
    assert!(!w.sentence.contains("unknown"), "{}", w.sentence);
    // Controls.
    let busy = words(
        &slot_of(
            facts(
                AgentPhase::Busy,
                MailFacts {
                    unread: 1,
                    ..MailFacts::default()
                },
            ),
            now,
        ),
        later,
        0,
    );
    let row = busy.fit::<TestHost>(120);
    assert!(row.contains("busy 3m35s"), "{row}");
    assert!(row.contains("\u{2709}1"), "{row}");
    let queued = words(
        &slot_of(
            facts(
                AgentPhase::Busy,
                MailFacts {
                    queued: 2,
                    ..MailFacts::default()
                },
            ),
            now,
        ),
        later,
        0,
    );
    assert_eq!(
        queued.mail, "\u{2709}0 \u{2191}2",
        "mail on its way still shows"
    );
}

/// The program's own report (OSC 7501, `Facts::program`) is the row's word
/// where no agent reading names a phase, ahead of the shell's; a phase the
/// screen's reader names keeps the slot (the report is display, never the
/// verdict).
#[test]
fn the_programs_own_report_is_shown_only_where_the_reader_names_no_phase() {
    let now = Instant::now();
    let later = now + Duration::from_secs(65);
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
        let row = words(&slot_of(facts(phase), now), later, 0).fit::<TestHost>(120);
        assert!(row.contains("codex working 1m05s"), "{row}");
        assert!(!row.contains("running"), "{row}");
    }
    // Control: a phase the reader names keeps the slot.
    let row = words(&slot_of(facts(Some(AgentPhase::Busy)), now), later, 0).fit::<TestHost>(120);
    assert!(row.contains("busy"), "{row}");
    assert!(!row.contains("codex working"), "{row}");
    // Control: with no report the shell's word shows.
    let quiet = TFacts {
        program: None,
        ..facts(None)
    };
    let row = words(&slot_of(quiet, now), later, 0).fit::<TestHost>(120);
    assert!(row.contains("running 1m05s"), "{row}");
}

/// A background codex finishing its turn (OSC 7501 `working` → `done`, the
/// host's count of them) puts the story dot on its tab; a tab the person is
/// looking at (its watermark at the story) shows none. The count standing
/// at the slot's first read is the baseline, not news, and a turn the ledger
/// settled in the same refresh is told once.
#[test]
fn a_programs_own_turn_end_puts_the_story_dot_on_a_background_tab() {
    let now = Instant::now();
    // Each end follows its own start.
    let at = |n: u64| TFacts {
        program_turn_ends: n,
        program_turn_starts: n,
        ..TFacts::default()
    };
    let mut s = slot_of(at(3), now);
    assert_eq!(s.story_seq, 0, "the count at the mint is the baseline");
    assert_eq!(s.chip(0), ChipLevel::Off);
    assert!(s.absorb(at(4), now + Duration::from_secs(1)));
    assert_eq!(s.story_seq, 1);
    assert_eq!(s.level(0), Level::Story);
    assert_eq!(s.chip(0), ChipLevel::Story, "the background tab's dot");
    assert_eq!(
        s.chip(s.story_seq),
        ChipLevel::Off,
        "the tab the person is looking at has seen it"
    );
    let row = words(&s, now + Duration::from_secs(2), 0).fit::<TestHost>(120);
    assert!(row.contains("1 turn"), "{row}");
    // No new end, no new point.
    assert!(!s.absorb(at(4), now + Duration::from_secs(3)));
    assert_eq!(s.story_seq, 1);
    // A ledger turn settling in the same refresh is the one point.
    let turn = TurnFact {
        id: 7,
        settled: true,
        dur_ms: 10,
        carried: false,
    };
    s.absorb(
        TFacts {
            turn: Some(turn),
            ..at(5)
        },
        now + Duration::from_secs(4),
    );
    assert_eq!(s.story_seq, 2, "told once, not twice");
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
    };
    // `starts` turns begun and `ends` ended by the program; the ledger's last.
    let at = |starts: u64, ends: u64, turn: Option<TurnFact>| TFacts {
        program_turn_starts: starts,
        program_turn_ends: ends,
        turn,
        ..TFacts::default()
    };
    let turns = |s: &Slot<TestHost>| {
        let row = words(s, t(9), 0).fit::<TestHost>(120);
        (s.story_seq, row)
    };

    // The settle first, the program's end a sweep later.
    let mut s = slot_of(at(1, 0, None), now);
    assert!(s.absorb(at(1, 0, Some(ledger(1, true))), t(1)));
    assert!(s.absorb(at(1, 1, Some(ledger(1, true))), t(2)));
    let (seq, row) = turns(&s);
    assert_eq!(seq, 1, "settle, then done: {row}");
    assert!(row.contains("1 turn"), "{row}");

    // The program's end first, the settle a sweep later.
    let mut s = slot_of(at(1, 0, None), now);
    assert!(s.absorb(at(1, 1, None), t(1)));
    assert!(s.absorb(at(1, 1, Some(ledger(1, true))), t(2)));
    let (seq, row) = turns(&s);
    assert_eq!(seq, 1, "done, then settle: {row}");
    assert!(row.contains("1 turn"), "{row}");
    assert_eq!(s.settled_at, Some(t(2)), "the settle still glows");

    // Both in one refresh.
    let mut s = slot_of(at(1, 0, None), now);
    assert!(s.absorb(at(1, 1, Some(ledger(1, true))), t(1)));
    assert_eq!(s.story_seq, 1, "one refresh");

    // Control: the program began a turn of its own after the settle; its end
    // is a second turn.
    let mut s = slot_of(at(1, 0, None), now);
    s.absorb(at(1, 0, Some(ledger(1, true))), t(1));
    s.absorb(at(2, 1, Some(ledger(1, true))), t(2));
    let (seq, row) = turns(&s);
    assert_eq!(seq, 2, "{row}");
    assert!(row.contains("2 turns"), "{row}");

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
    let (seq, row) = turns(&s);
    assert_eq!(seq, 2, "an end before the ledger turn opened: {row}");
    assert!(row.contains("2 turns"), "{row}");
}

/// A turn already recorded under aterm's own hand stays omitted when its
/// delayed counterpart arrives after that hand was released. A watched foreign
/// point is different: pairing may make that retained point news. Both orders
/// use the actual fold and story/level queries, with no renderer or live host.
#[test]
fn a_paired_owned_turn_does_not_leave_news_without_a_story() {
    let now = Instant::now();
    let t = |s: u64| now + Duration::from_secs(s);
    let ledger = Some(TurnFact {
        id: 1,
        settled: true,
        dur_ms: 5_000,
        carried: false,
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
/// story without being news: no `Story` level, so no row is born (a re-grid
/// as the turn ends) to fold at the next key (a second one). NEGATIVE
/// CONTROL: the same end unwatched is news.
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
    assert!(!s.level(0).shows_row(), "so no row is born");
    assert_eq!(s.chip(0), ChipLevel::Off);
    assert!(s.absorb(at(2, false), now + Duration::from_secs(2)));
    assert_eq!(s.level(0), Level::Story, "unwatched, it is news");
}

/// The width law: rtt, since, role, ctx, then the mail's and the hand's
/// details shed, so phase, hand and mail survive to 24 columns; below that
/// the three are cut, all on the row down to 13 columns; nothing is ever
/// wider than asked.
#[test]
fn elision_keeps_phase_hand_and_mail_to_24_and_all_three_to_13() {
    let now = Instant::now();
    let w = words(&driven(now), now + Duration::from_secs(192), 0);
    assert!(!w.fit::<TestHost>(90).contains('\u{27df}'), "rtt first");
    assert!(!w.fit::<TestHost>(83).contains("3m12s"), "then since");
    assert!(!w.fit::<TestHost>(75).contains("worker"), "then role");
    assert!(!w.fit::<TestHost>(50).contains("ctx"), "then ctx");
    assert!(
        !w.fit::<TestHost>(40).contains("task"),
        "then the mail detail"
    );
    assert!(
        !w.fit::<TestHost>(28).contains("manager"),
        "then the hand's holder"
    );
    let n24 = w.fit::<TestHost>(24);
    assert_eq!(n24, "busy  \u{25c2} turn 41  \u{2709}2");
    for cols in 13..=24 {
        let kinds: Vec<SlotKind> = w.pieces::<TestHost>(cols).iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            [SlotKind::Phase, SlotKind::Hand, SlotKind::Mail],
            "{cols}: {}",
            w.fit::<TestHost>(cols)
        );
    }
    for cols in 0..=200 {
        assert!(
            w.fit::<TestHost>(cols).chars().count() <= cols,
            "{cols}: {}",
            w.fit::<TestHost>(cols)
        );
    }
}

/// No free-text value may carry the row's glyphs, a bidi override or a
/// control byte, and the cap cuts with `…`.
#[test]
fn sanitize_refuses_band_glyphs_bidi_and_control_characters() {
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
        '\u{200e}', '\u{200f}', '\u{202a}', '\u{2069}', '\u{1b}', '~', '\u{2192}',
    ] {
        assert_eq!(
            sanitize_token::<TestHost>(&format!("a{c}b"), 8),
            "ab",
            "{c:?}"
        );
    }
    // Every free-text slot goes through it.
    let now = Instant::now();
    let w = words(
        &slot_of(
            TFacts {
                role: Some("\u{25c2} fake hand".into()),
                hand: Hand::DrivenLease {
                    holder: "\u{2709}9 boss".into(),
                    pid: None,
                },
                ..TFacts::default()
            },
            now,
        ),
        now,
        0,
    );
    assert_eq!(w.role, "fake hand");
    assert_eq!(w.hand, "\u{25c2} 9 boss");
    // The hold reason is decoded by the host's codec, then sanitized.
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
/// filler, CGJ) on every path a free-text value takes: the token, a hold
/// reason the host's codec decoded, and the slots and sentence it lands in.
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
    }
    let smuggled = format!("ok{}end", hidden.iter().collect::<String>());
    assert_eq!(sanitize_token::<TestHost>(&smuggled, 64), "okend");
    // A hold reason the host decodes (`%20` is a space here) is held to it.
    assert_eq!(
        crate::presence::hold_reason_words::<TestHost>(
            "main%20broken\u{e0049}\u{e0047}\u{200b}\u{feff}"
        ),
        "main broken"
    );
    let now = Instant::now();
    let w = words(
        &slot_of(
            TFacts {
                role: Some("rev\u{e0049}\u{e0047}iewer".into()),
                hand: Hand::DrivenLease {
                    holder: "boss\u{200b}\u{2060}".into(),
                    pid: None,
                },
                ..TFacts::default()
            },
            now,
        ),
        now,
        0,
    );
    assert_eq!(w.role, "reviewer");
    assert_eq!(w.hand, "\u{25c2} boss");
    for (name, text) in [
        ("fit", w.fit::<TestHost>(200)),
        ("sentence", w.sentence.clone()),
    ] {
        assert!(
            !text.chars().any(|c| hidden.contains(&c)),
            "{name}: {text:?}"
        );
    }
}

/// The painter: exactly `cols` cells at every width, the text is the fitted
/// line inside one margin cell each side, and the emoji-capable glyphs are
/// pinned to text presentation.
#[test]
fn paint_row_is_exactly_cols_wide_with_the_glyphs_in_text_presentation() {
    let now = Instant::now();
    let c = BandInks::derive(
        crate::ink::ThemeInks {
            bg: [0x1e, 0x1e, 0x2e],
            fg: [0xcd, 0xd6, 0xf4],
            cursor: [0xf5, 0xe0, 0xdc],
        },
        None,
        crate::ink::BarBase::Blend,
    );
    let tones = crate::presence::tones([0x1e, 0x1e, 0x2e], &c, None);
    let p: PresenceTones = crate::presence::inks(tones, c.bar_bg);
    let mut s = driven(now);
    s.absorb(
        TFacts {
            hold: Some(crate::presence::HoldFact {
                reason: "review".into(),
                fleet: true,
            }),
            ..driven_facts()
        },
        now,
    );
    let w: Words = words(&s, now, 0);
    for cols in [0usize, 1, 2, 3, 13, 24, 40, 80, 120, 200] {
        let row: Vec<InkedCell> = paint_row::<TestHost>(&w, cols, &c, &p);
        assert_eq!(row.len(), cols);
        let text: String = row.iter().map(|k| k.ch).collect();
        assert_eq!(text.trim(), w.fit::<TestHost>(text_cols(cols)), "{cols}");
        for k in &row {
            assert_eq!(k.bg, c.bar_bg, "on the band");
            assert_eq!(
                k.text_presentation,
                matches!(
                    k.ch,
                    '\u{2713}' | '\u{2717}' | '\u{26a0}' | '\u{2709}' | '\u{2715}' | '\u{1f512}'
                ),
                "{:?}",
                k.ch
            );
            // The row's own glyphs are one cell each under the grid's
            // measure too (the lock by its pin): no continuation.
            assert!(!k.continuation, "{cols}: {:?}", k.ch);
        }
    }
    let row = paint_row::<TestHost>(&w, 120, &c, &p);
    let hold = row.iter().find(|k| k.ch == '\u{2298}').expect("the hold");
    assert_eq!(hold.fg, p.stop);
    assert!(hold.bold);
}

/// The band's inks and the presence text inks on a dark theme.
fn band_inks() -> (BandInks, PresenceTones) {
    let c = BandInks::derive(
        crate::ink::ThemeInks {
            bg: [0x1e, 0x1e, 0x2e],
            fg: [0xcd, 0xd6, 0xf4],
            cursor: [0xf5, 0xe0, 0xdc],
        },
        None,
        crate::ink::BarBase::Blend,
    );
    let tones = crate::presence::tones([0x1e, 0x1e, 0x2e], &c, None);
    let p = crate::presence::inks(tones, c.bar_bg);
    (c, p)
}

/// A row with only `role` free: phase `busy`, no hand.
fn role_row(role: &str) -> Words {
    Words {
        role: sanitize_token::<TestHost>(role, 64),
        phase: "busy".into(),
        hand: "\u{2014}".into(),
        hand_short: "\u{2014}".into(),
        ..Words::default()
    }
}

/// `line` as the terminal grid draws it, one `(char, continuation)` per
/// column: each character that opens a cell, a wide one followed by its
/// continuation, and nothing for a character drawn in the cell before it.
/// Walked with the grid's own `GridColumns`, independent of the row's
/// measure.
fn grid_cells(line: &str) -> Vec<(char, bool)> {
    let mut grid = aterm_grapheme::GridColumns::new();
    let mut out = Vec::new();
    for c in line.chars() {
        match grid.step(c) {
            aterm_grapheme::GridStep::Opens { width, .. } => {
                out.push((c, false));
                out.extend(std::iter::repeat_n((' ', true), width - 1));
            }
            aterm_grapheme::GridStep::Joins { widened: true } => out.push((' ', true)),
            _ => {}
        }
    }
    out
}

/// The width law measures in the grid's cells: a role of seven CJK
/// characters is fourteen columns, so at 22 (`レビュー担当者  busy  —` is 23) it
/// is shed; a combining mark is no column, so `cafe\u{301} lead` keeps its
/// slot in exactly 18. The token cap counts cells and cuts between two of
/// them, and at every width the line fits the grid's own count.
#[test]
fn a_cjk_or_combining_role_is_laid_out_in_the_grids_cells() {
    let cjk = role_row("\u{30ec}\u{30d3}\u{30e5}\u{30fc}\u{62c5}\u{5f53}\u{8005}");
    assert_eq!(
        cjk.fit::<TestHost>(23),
        "\u{30ec}\u{30d3}\u{30e5}\u{30fc}\u{62c5}\u{5f53}\u{8005}  busy  \u{2014}"
    );
    assert_eq!(
        cjk.fit::<TestHost>(22),
        "busy  \u{2014}",
        "the role is 14 columns"
    );
    let marked = role_row("cafe\u{301} lead");
    assert_eq!(
        marked.fit::<TestHost>(18),
        "cafe\u{301} lead  busy  \u{2014}",
        "the mark takes no column"
    );
    assert_eq!(marked.fit::<TestHost>(17), "busy  \u{2014}");
    // The cap: 8 cells hold three wide glyphs and the `…`, never half a
    // fourth; a mark stays with its letter at the cut.
    assert_eq!(
        sanitize_token::<TestHost>("\u{30ec}\u{30d3}\u{30e5}\u{30fc}\u{62c5}", 8),
        "\u{30ec}\u{30d3}\u{30e5}\u{2026}"
    );
    assert_eq!(
        sanitize_token::<TestHost>("abcde\u{301}fgh", 6),
        "abcde\u{301}\u{2026}"
    );
    // Every width, every shape: the fitted line is never wider than asked in
    // the grid's own count, and a cut never splits a cell.
    for role in [
        "\u{30ec}\u{30d3}\u{30e5}\u{30fc}\u{62c5}\u{5f53}\u{8005}",
        "cafe\u{301} lead",
        "dev \u{1f468}\u{200d}\u{1f4bb}",
        "\u{d55c}\u{ad6d}\u{c5b4} \u{c5ed}\u{d560} reviewer",
        "\u{a9}\u{fe0f} keeper",
    ] {
        let w = Words {
            hand: format!(
                "\u{25c2} {} \u{00b7} turn 4",
                sanitize_token::<TestHost>(role, 32)
            ),
            hand_short: "\u{25c2} turn 4".into(),
            since: vec![sanitize_token::<TestHost>(role, 48), "2m03s".into()],
            ..role_row(role)
        };
        for cols in 0..=80 {
            let line = w.fit::<TestHost>(cols);
            assert!(
                aterm_grapheme::str_grid_columns(&line) <= cols,
                "{role:?} at {cols}: {line:?}"
            );
        }
        // Where the grid opens a cell in the clean token: the only places a
        // cut may fall.
        let clean = sanitize_token::<TestHost>(role, usize::MAX);
        let mut grid = aterm_grapheme::GridColumns::new();
        let opens: Vec<usize> = clean
            .char_indices()
            .filter(|&(_, c)| matches!(grid.step(c), aterm_grapheme::GridStep::Opens { .. }))
            .map(|(at, _)| at)
            .collect();
        for cap in 0..=40 {
            let t = sanitize_token::<TestHost>(role, cap);
            assert!(
                aterm_grapheme::str_grid_columns(&t) <= cap,
                "{role:?} cap {cap}: {t:?}"
            );
            if let Some(head) = t.strip_suffix('\u{2026}') {
                assert!(
                    clean.starts_with(head) && opens.contains(&head.len()),
                    "{role:?} cap {cap}: {t:?} cuts inside a cell"
                );
            }
        }
    }
}

/// The painter draws each word in the cells the grid gives it (a wide glyph
/// as its lead cell and a continuation, a combining mark in none), so the
/// painted row is the fitted line as the grid lays it out at every width.
#[test]
fn the_painter_draws_a_wide_glyph_in_two_cells_and_a_mark_in_none() {
    let (c, p) = band_inks();
    let row = paint_row::<TestHost>(&role_row("\u{62c5}\u{5f53}"), 40, &c, &p);
    let painted: Vec<(char, bool)> = row.iter().map(|k| (k.ch, k.continuation)).collect();
    assert_eq!(
        &painted[..11],
        &[
            (' ', false),
            ('\u{62c5}', false),
            (' ', true),
            ('\u{5f53}', false),
            (' ', true),
            (' ', false),
            (' ', false),
            ('b', false),
            ('u', false),
            ('s', false),
            ('y', false),
        ]
    );
    let row = paint_row::<TestHost>(&role_row("cafe\u{301}"), 40, &c, &p);
    assert!(row.iter().all(|k| k.ch != '\u{301}'), "no cell of its own");
    let text: String = row.iter().map(|k| k.ch).collect();
    assert!(text.starts_with(" cafe  busy  \u{2014}"), "{text:?}");
    for role in [
        "\u{30ec}\u{30d3}\u{30e5}\u{30fc}\u{62c5}\u{5f53}\u{8005}",
        "cafe\u{301} lead",
        "dev \u{1f468}\u{200d}\u{1f4bb}",
        "\u{a9}\u{fe0f} keeper",
    ] {
        let w = role_row(role);
        for cols in 0..=60 {
            let row = paint_row::<TestHost>(&w, cols, &c, &p);
            assert_eq!(row.len(), cols);
            let mut want = vec![(' ', false)];
            want.extend(grid_cells(&w.fit::<TestHost>(text_cols(cols))));
            want.resize(cols.max(want.len()), (' ', false));
            want.truncate(cols);
            let got: Vec<(char, bool)> = row.iter().map(|k| (k.ch, k.continuation)).collect();
            assert_eq!(got, want, "{role:?} at {cols}");
            for k in row.iter().filter(|k| k.continuation) {
                assert!(!k.text_presentation && k.bg == c.bar_bg);
            }
        }
    }
}

/// A cell holds what the grid's cell holds, its opening character and 16
/// more (aterm-grid's `MAX_COMBINING`), and the row drops the rest. A hostile
/// reset of `1` then 190 `~` (a glyph the token strips) each carrying 16
/// marks is one cell: the token is `1` and its first 16 marks. A token is at
/// most 17 characters a cell of its cap, a mark with no cell to join is
/// dropped, and the row measures and paints every cell as the grid does.
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
    // A zero-width character with no cell to join — at the front, or left
    // there by a stripped glyph — is kept by neither the grid nor the token.
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
        let t = sanitize_token::<TestHost>(&many, cap);
        assert!(t.chars().count() <= 17 * cap, "cap {cap}: {t:?}");
        assert!(aterm_grapheme::str_grid_columns(&t) <= cap, "cap {cap}");
    }
    let (c, p) = band_inks();
    for token in [one, sanitize_token::<TestHost>(&many, 32)] {
        let w = Words {
            since: vec![format!("\u{2192} {token}"), "1h59m".into()],
            ..role_row("x")
        };
        let full = w.fit::<TestHost>(200);
        assert!(full.contains(&token), "{full:?}");
        for cols in 0..=80 {
            let line = w.fit::<TestHost>(cols);
            assert!(aterm_grapheme::str_grid_columns(&line) <= cols, "{cols}");
            let row = paint_row::<TestHost>(&w, cols, &c, &p);
            let mut want = vec![(' ', false)];
            want.extend(grid_cells(&w.fit::<TestHost>(text_cols(cols))));
            want.resize(cols.max(want.len()), (' ', false));
            want.truncate(cols);
            let got: Vec<(char, bool)> = row.iter().map(|k| (k.ch, k.continuation)).collect();
            assert_eq!(got, want, "at {cols}");
        }
    }
}

/// The walk of the row's cells is linear in what it is given: each character
/// is measured against its own cell, which holds at most 17, so the token,
/// the fit and the painter measure a bounded few characters per character
/// (counted in what the host measured, not a clock), even on a raw string of
/// one cell with 4,096 marks.
#[test]
fn the_cell_walk_is_linear_in_its_string() {
    let (c, p) = band_inks();
    for n in [256usize, 4096] {
        let raw = format!("a{}", "\u{301}".repeat(n));
        let w = Words {
            role: raw.clone(),
            ..role_row("x")
        };
        for (name, work) in [
            (
                "sanitize",
                measured(|| drop(sanitize_token::<TestHost>(&raw, 32))),
            ),
            ("fit", measured(|| drop(w.fit::<TestHost>(200)))),
            (
                "paint",
                measured(|| drop(paint_row::<TestHost>(&w, 200, &c, &p))),
            ),
        ] {
            assert!(
                work <= 8 * (n + 1),
                "{name}: {work} characters measured for {}",
                n + 1
            );
        }
    }
}

/// The phase slot for a published input stall: `frozen <dur> · not reading
/// input` and its spoken remedy, the survivor's `end it with signal kill`,
/// and `stopped <dur> · input queued` for a stopped job. Where the host
/// would carry the restart ([`Host::stall_restart`]) both frozen remedies
/// name it instead, the printed slot unchanged; a stopped job's stays
/// `resume it`. The duration counts from the oldest unread byte.
#[test]
fn the_stall_says_frozen_or_stopped_with_the_age_and_the_remedy() {
    let now = Instant::now();
    let later = now + Duration::from_secs(9660);
    let act = "aterm ctl @s-1 restart pid=4242 if-stalled";
    for (stopped, survived, restart, phase, why, remedy) in [
        (
            false,
            false,
            None,
            "frozen",
            "not reading input",
            "restart it",
        ),
        (false, false, Some(act), "frozen", "not reading input", act),
        (
            false,
            true,
            None,
            "frozen",
            "survived its restart",
            "signal kill",
        ),
        (
            false,
            true,
            Some(act),
            "frozen",
            "survived its restart",
            act,
        ),
        (true, false, None, "stopped", "input queued", "resume it"),
        (
            true,
            false,
            Some(act),
            "stopped",
            "input queued",
            "resume it",
        ),
    ] {
        let case = format!("stopped={stopped} survived={survived} restart={restart:?}");
        let (p, since, spoken) = stall_words(now, stopped, survived, restart, later);
        assert_eq!(p, phase, "{case}");
        assert_eq!(since, ["2h41m", why], "{case}");
        assert!(spoken.contains(remedy), "{case}: {spoken}");
        assert_eq!(spoken.contains("2h41m"), !survived, "{case}: {spoken}");
        if stopped {
            assert!(
                !spoken.contains(act),
                "{case}: a stopped job is resumed: {spoken}"
            );
        }
    }
}

/// Which `since` clauses print seconds (and so tick every second): a token
/// ending in `s` right after a digit. `3 turns`, `2h05m`, `1d 22h` and a
/// reset time do not, and a row of only those ticks once a minute.
#[test]
fn a_clause_prints_seconds_only_on_a_digit_then_s() {
    assert!(prints_seconds("3m12s"));
    assert!(prints_seconds("since 40s"));
    assert!(prints_seconds("held 1m00s, resumed 1m00s ago"));
    assert!(!prints_seconds("2h05m"));
    assert!(!prints_seconds("1d 22h"));
    assert!(!prints_seconds("3 turns"));
    assert!(!prints_seconds("\u{2192} 19:30"));
    let row = |since: &[&str]| Words {
        since: since.iter().map(|c| (*c).to_string()).collect(),
        ..Words::default()
    };
    assert_eq!(
        words_step(&row(&["2h05m", "3 turns"])),
        Duration::from_secs(60)
    );
    assert_eq!(words_step(&row(&["2h05m", "12s"])), Duration::from_secs(1));
}

// ---- slot and words --------------------------------------------------------
// Over this file's seam: the usage wall is `TestWall::Usage`, the input stall
// a `TestStall`, the slot and facts `TSlot`/`TFacts`.

/// The six slots in order: role, phase since, hand, mail with the trust
/// glyph before the sender, ctx, fabric.
#[test]
fn the_six_slots_read_like_the_approved_mock() {
    let now = Instant::now();
    let s = driven(now);
    let w = words(&s, now + Duration::from_secs(192), 0);
    assert_eq!(
        w.fit::<TestHost>(120),
        "worker:claude-satcomp  busy 3m12s  \u{25c2} manager \u{00b7} turn 41  \u{2709}2 task\u{2190}\u{2713}manager  ctx 41% left  \u{27df} 12ms"
    );
    assert!(
        w.sentence.contains("driven by manager") && w.sentence.contains("2 unread"),
        "{}",
        w.sentence
    );
    assert_eq!(w.tone, Tone::Info);
    assert_eq!(s.level(0), Level::Driven);
    assert_eq!(s.level(0).rim(), Rim::Drive);
}

/// One place tells it: an attention whose only owner is the agent upgrade is
/// still a fact (`level=attention`, `why=escalation`, the tab chip's wait
/// mark), but the presence line's phase slot does not repeat words the
/// message band already says. Negative control: the same text from any other
/// owner reads on the line.
#[test]
fn an_upgrade_only_attention_keeps_its_level_and_chip_but_not_its_words() {
    let now = Instant::now();
    let text = "Codex 0.157.0 \u{2192} 0.157.1 stalled";
    let mut s = TSlot::new(now);
    s.absorb(
        TFacts {
            attention: Some(text.into()),
            attention_told_elsewhere: true,
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(s.level(0), Level::Attention);
    assert_eq!(s.why(0), "escalation");
    assert_eq!(s.chip(0), ChipLevel::Wait);
    // The window's own chrome shows nothing for it: no rim and no presence
    // row, since the line has no words to say.
    assert_eq!(s.shown_level(0), Level::Quiet);
    assert_eq!(s.shown_level(0).rim(), Rim::None);
    assert!(!s.shown_level(0).shows_row());
    assert!(!s.calm(), "the stall is not folded by a keystroke");
    let w = words(&s, now, 0);
    assert!(
        !w.phase.contains("Codex") && !w.sentence.contains("Codex"),
        "the line repeats the band's words: {:?} / {:?}",
        w.phase,
        w.sentence
    );
    // Negative control: another owner's attention is the line's phase.
    let mut other = TSlot::new(now);
    other.absorb(
        TFacts {
            attention: Some(text.into()),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(other.level(0), Level::Attention);
    assert_eq!(other.shown_level(0), Level::Attention);
    assert_eq!(other.shown_level(0).rim(), Rim::Wait);
    assert!(words(&other, now, 0).phase.contains("Codex"));
    // The flag moving alone is a change the view must re-read.
    assert!(other.absorb(
        TFacts {
            attention: Some(text.into()),
            attention_told_elsewhere: true,
            ..TFacts::default()
        },
        now,
    ));
    assert!(!words(&other, now, 0).phase.contains("Codex"));
}

/// An attention told elsewhere never takes the row's words or colour: beside
/// an upgrade stall the band tells and a story since the watermark, the row
/// is shown at the story's level with the story's summary, its tone not
/// Warn. Negative control: the same story beside another owner's attention
/// reads that attention's words, in Warn.
#[test]
fn a_story_beside_an_attention_told_elsewhere_keeps_its_words_and_tone() {
    let now = Instant::now();
    let text = "Claude 2.1.281 \u{2192} 2.1.282 stalled";
    let slot = |elsewhere: bool| {
        let mut s = TSlot::new(now);
        for (id, at) in [(1, 5), (2, 70)] {
            s.absorb(
                TFacts {
                    turn: Some(TurnFact {
                        id,
                        settled: true,
                        dur_ms: 10,
                        carried: false,
                    }),
                    attention: Some(text.into()),
                    attention_told_elsewhere: elsewhere,
                    ..TFacts::default()
                },
                now + Duration::from_secs(at),
            );
        }
        s
    };
    let later = now + Duration::from_secs(130);
    let s = slot(true);
    assert_eq!(s.level(0), Level::Attention, "the fact stands");
    assert_eq!(s.shown_level(0), Level::Story);
    let w = words(&s, later, 0);
    assert!(
        w.since.iter().any(|c| c.ends_with("turns")),
        "the story's summary: {:?}",
        w.since
    );
    assert_ne!(w.tone, Tone::Warn);
    assert_ne!(s.tone(later, 0), Tone::Warn);

    let other = slot(false);
    assert_eq!(other.shown_level(0), Level::Attention);
    let w = words(&other, later, 0);
    assert!(w.phase.contains("stalled"), "{:?}", w.phase);
    assert_eq!(w.tone, Tone::Warn);
}

/// Severity: hold > limited > attention > driven > story > quiet, and the
/// rim follows the level exactly: a colour with no words is unreachable.
#[test]
fn severity_and_rim_follow_the_design_table() {
    let now = Instant::now();
    let mut s = TSlot::new(now);
    assert_eq!(s.level(0), Level::Quiet);
    assert_eq!(s.level(0).rim(), Rim::None);
    s.absorb(
        TFacts {
            hand: Hand::DrivenTurn {
                id: 1,
                holder: None,
            },
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(s.level(0), Level::Driven);
    s.absorb(
        TFacts {
            hand: Hand::DrivenTurn {
                id: 1,
                holder: None,
            },
            attention: Some("needs a decision".into()),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(s.level(0), Level::Attention);
    assert_eq!(s.level(0).rim(), Rim::Wait);
    s.absorb(
        TFacts {
            hand: Hand::DrivenTurn {
                id: 1,
                holder: None,
            },
            attention: Some("needs a decision".into()),
            agent_seq: 2,
            agent: Some(AgentReading {
                phase: AgentPhase::Wall {
                    kind: TestWall::Usage,
                    reset: Some("19:30".into()),
                    until: None,
                },
                context_pct: None,
            }),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(s.level(0), Level::Limited);
    assert_eq!(s.level(0).rim(), Rim::Stop { hold: false });
    assert_eq!(s.chip(0), ChipLevel::Stop(StopCause::Wall));
    s.absorb(
        TFacts {
            hold: Some(HoldFact {
                reason: "fabric-lost".into(),
                fleet: true,
            }),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(s.level(0), Level::Hold);
    assert_eq!(s.level(0).rim(), Rim::Stop { hold: true });
    let w = words(&s, now + Duration::from_secs(40), 0);
    assert!(
        w.hand
            .starts_with("\u{2298} hold fabric-lost \u{00b7}fleet\u{1f512}"),
        "{}",
        w.hand
    );
    assert!(
        w.sentence
            .starts_with("held, fabric-lost, fleet, cannot be lifted here"),
        "{}",
        w.sentence
    );
    assert_eq!(w.tone, Tone::Warn);
    // A stalled bridge is an instance fact: fabric slot, never a rim.
    let mut q = TSlot::new(now);
    q.absorb(
        TFacts {
            link: Link::Stalled {
                age_ms: Some(7_400),
            },
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(q.level(0), Level::Note);
    assert_eq!(q.level(0).rim(), Rim::None);
    assert_eq!(words(&q, now, 0).fabric, "~ 7s");
    let mut lost = TSlot::new(now);
    lost.absorb(
        TFacts {
            link: Link::Disconnected,
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(lost.level(0).rim(), Rim::None);
    assert_eq!(words(&lost, now, 0).fabric, "\u{2715} lost");
}

/// An approval box, the supervisor's "answer this box" as typed attention, a
/// supervisor's turn on the hand, and a published input stall: the slot
/// stands at `Limited` (the stop rim), is not calm, and its phase reads
/// `frozen` with the stall's age and why, ahead of the attention text. A
/// stopped job reads `stopped` and says to resume it. Negative control: with
/// the stall cleared the box's attention is back, at `Attention`.
#[test]
fn a_stall_is_limited_not_calm_and_reads_frozen_over_typed_attention() {
    let now = Instant::now();
    let fact = TestStall {
        since: now,
        stopped: false,
        survived: false,
        restart: None,
    };
    let facts = |stall: Option<TestStall>| TFacts {
        attention: Some("answer this box: 4. Chat about this".into()),
        agent_seq: 1,
        agent: Some(AgentReading {
            phase: AgentPhase::Prompt {
                detail: Some("question".into()),
            },
            context_pct: None,
        }),
        hand: Hand::DrivenTurn {
            id: 3,
            holder: Some("supervisor".into()),
        },
        input_stall: stall,
        ..TFacts::default()
    };
    let mut s = TSlot::new(now);
    assert!(s.absorb(facts(Some(fact.clone())), now));
    assert_eq!(s.level(0), Level::Limited);
    assert_eq!(s.level(0).rim(), Rim::Stop { hold: false });
    assert_eq!(s.chip(0), ChipLevel::Stop(StopCause::Frozen));
    assert!(!s.calm());
    assert_eq!(s.why(0), "-");
    let later = now + Duration::from_secs(123);
    let w = words(&s, later, 0);
    assert_eq!(w.phase, "frozen");
    assert_eq!(
        w.since,
        vec!["2m03s".to_string(), "not reading input".to_string()]
    );
    assert!(
        w.fit::<TestHost>(120)
            .contains("frozen 2m03s \u{00b7} not reading input"),
        "{}",
        w.fit::<TestHost>(120)
    );
    assert!(
        !w.fit::<TestHost>(120).contains("answer this box"),
        "{}",
        w.fit::<TestHost>(120)
    );
    assert!(
        w.sentence
            .contains("frozen, not reading input for 2m03s; restart it"),
        "{}",
        w.sentence
    );
    assert_eq!(w.tone, Tone::Warn);
    // A stopped job: resume it.
    let stopped = TestStall {
        stopped: true,
        ..fact
    };
    assert!(s.absorb(facts(Some(stopped)), later));
    let w = words(&s, now + Duration::from_secs(41), 0);
    assert_eq!(w.phase, "stopped");
    assert_eq!(w.since, vec!["41s".to_string(), "input queued".to_string()]);
    assert!(
        w.sentence
            .contains("stopped with input queued for 41s; resume it"),
        "{}",
        w.sentence
    );
    assert_eq!(s.level(0), Level::Limited);
    assert_eq!(s.chip(0), ChipLevel::Stop(StopCause::Suspended));
    // Negative control: the stall clears and the box's attention is back.
    assert!(s.absorb(facts(None), later));
    assert_eq!(s.level(0), Level::Attention);
    assert_eq!(s.why(0), "prompt,escalation");
    let w = words(&s, later, 0);
    assert!(w.phase.starts_with("answer this box"), "{}", w.phase);
    // A hold still outranks it.
    let mut held = TSlot::new(now);
    held.absorb(
        TFacts {
            hold: Some(HoldFact {
                reason: "review".into(),
                fleet: false,
            }),
            ..facts(Some(TestStall {
                since: now,
                stopped: false,
                survived: false,
                restart: None,
            }))
        },
        now,
    );
    assert_eq!(held.level(0), Level::Hold);
}

/// The story row: quiet, with a since-summary of what happened after the
/// watermark — and it folds to Quiet once the watermark catches up.
#[test]
fn a_story_summarises_since_the_watermark_and_the_watermark_folds_it() {
    let now = Instant::now();
    let mut s = TSlot::new(now);
    for id in 1..=3 {
        s.absorb(
            TFacts {
                turn: Some(TurnFact {
                    id,
                    settled: true,
                    dur_ms: 10,
                    carried: false,
                }),
                ..TFacts::default()
            },
            now + Duration::from_secs(id),
        );
    }
    s.absorb(
        TFacts {
            turn: Some(TurnFact {
                id: 3,
                settled: true,
                dur_ms: 10,
                carried: false,
            }),
            mail: MailFacts {
                unread: 0,
                head: 2,
                ..MailFacts::default()
            },
            ..TFacts::default()
        },
        now + Duration::from_secs(5),
    );
    s.absorb(
        TFacts {
            turn: Some(TurnFact {
                id: 3,
                settled: true,
                dur_ms: 10,
                carried: false,
            }),
            mail: MailFacts {
                unread: 0,
                head: 2,
                ..MailFacts::default()
            },
            hold: Some(HoldFact {
                reason: "pause".into(),
                fleet: false,
            }),
            ..TFacts::default()
        },
        now + Duration::from_secs(10),
    );
    s.absorb(
        TFacts {
            turn: Some(TurnFact {
                id: 3,
                settled: true,
                dur_ms: 10,
                carried: false,
            }),
            mail: MailFacts {
                unread: 0,
                head: 2,
                ..MailFacts::default()
            },
            ..TFacts::default()
        },
        now + Duration::from_secs(70),
    );
    assert_eq!(s.level(0), Level::Story);
    assert_eq!(s.chip(0), ChipLevel::Story);
    let w = words(&s, now + Duration::from_secs(130), 0);
    assert_eq!(w.phase, "\u{25c7} quiet");
    assert_eq!(
        w.since,
        vec![
            "since 2m09s".to_string(),
            "3 turns".to_string(),
            "1 mail".to_string(),
            "held 1m00s, resumed 1m00s ago".to_string(),
        ]
    );
    // The longest stop survives elision: at 44 columns the counts go first.
    let narrow = w.fit::<TestHost>(44);
    assert!(narrow.contains("held 1m00s"), "{narrow}");
    assert!(s.calm());
    assert_eq!(s.level(s.story_seq), Level::Quiet);
    assert!(!s.level(s.story_seq).shows_row());
}

/// The story an agent told while nobody looked closes when the agent leaves:
/// a shell does not stand at `level=story` for a run that is over, least of
/// all headless, where no person acts to read it. A new point after the exit
/// is news again.
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
    assert_eq!(
        s.level(0),
        Level::Story,
        "the question is news while it runs"
    );
    assert!(s.story_since(0).next().is_some());
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
            }),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(s.level(0), Level::Story, "a point after the exit is news");
    assert_eq!(s.story_since(0).count(), 1);
}

/// The tone: Warn while a human should look, Success for 2 s after a
/// settled turn, Info otherwise.
#[test]
fn tone_is_warn_then_success_for_two_seconds_then_info() {
    let now = Instant::now();
    let mut s = TSlot::new(now);
    s.absorb(
        TFacts {
            turn: Some(TurnFact {
                id: 1,
                settled: true,
                dur_ms: 1,
                carried: false,
            }),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(s.tone(now + Duration::from_millis(500), 0), Tone::Success);
    assert_eq!(s.tone(now + Duration::from_millis(2500), 0), Tone::Info);
    s.absorb(
        TFacts {
            turn: Some(TurnFact {
                id: 1,
                settled: true,
                dur_ms: 1,
                carried: false,
            }),
            agent_seq: 1,
            agent: Some(AgentReading {
                phase: AgentPhase::Prompt {
                    detail: Some("bash".into()),
                },
                context_pct: Some(12),
            }),
            ..TFacts::default()
        },
        now,
    );
    assert_eq!(s.tone(now, 0), Tone::Warn);
    let w = words(&s, now, 0);
    assert_eq!(w.phase, "bash approval");
    assert_eq!(w.since, ["0s"], "the wait's age, as a question's");
    assert_eq!(w.ctx, "ctx 12% left \u{26a0}");
    // The classifier's verdict and an unnamed kind stay off the band.
    assert_eq!(
        prompt_band_word(Some("bash:not-read-only")),
        "bash approval"
    );
    assert_eq!(prompt_band_word(Some("other")), "approval");
    assert_eq!(prompt_band_word(None), "approval");
    // The question tool asks; it approves nothing.
    assert_eq!(prompt_band_word(Some("question")), "question");
    assert_eq!(prompt_band_word(Some("plan-exit")), "plan approval");
    assert_eq!(
        prompt_band_word(Some("read-outside-setting")),
        "outside read approval"
    );
    assert_eq!(prompt_band_word(Some("mcp-server")), "MCP server approval");
    // A box that follows another, with no busy reading between, starts
    // its own age; the same box's next reading keeps it.
    let box_of = |seq, detail: &str| TFacts {
        agent_seq: seq,
        agent: Some(AgentReading {
            phase: AgentPhase::Prompt {
                detail: Some(detail.into()),
            },
            context_pct: Some(12),
        }),
        ..TFacts::default()
    };
    let later = now + Duration::from_secs(65);
    s.absorb(box_of(2, "bash:not-read-only"), later);
    assert_eq!(words(&s, later, 0).since, ["1m05s"], "the same box");
    s.absorb(box_of(3, "edit"), later);
    let w = words(&s, later, 0);
    assert_eq!(w.phase, "edit approval");
    assert_eq!(w.since, ["0s"], "a new box's own age");
}

/// A retry's next try lapses with its time: once `until` is reached the wall
/// names its cause alone (no `→ 09:04`, no `next try`), since a plan that
/// lapses by the clock posts no new agent reading. Negative controls: a wall
/// that names a reset (a usage window) keeps its `→ 19:30` and `resets` after
/// the countdown ends, and a retry with time left prints both.
#[test]
fn a_retrys_next_try_lapses_with_its_time() {
    let now = Instant::now();
    let retry = |until: Option<Duration>| {
        slot_of(
            TFacts {
                agent_seq: 1,
                agent: agent(AgentPhase::Wall {
                    kind: TestWall::Api,
                    reset: Some("09:04".into()),
                    until: until.map(|d| now + d),
                }),
                ..TFacts::default()
            },
            now,
        )
    };
    let s = retry(Some(Duration::from_secs(120)));
    let w = words(&s, now, 0);
    assert_eq!(w.phase, "can't reach the API");
    assert_eq!(w.since, ["\u{2192} 09:04", "2m00s"], "{w:?}");
    assert!(w.sentence.contains("next try 09:04"), "{}", w.sentence);
    // Half a minute before it: still ahead.
    let w = words(&s, now + Duration::from_secs(90), 0);
    assert_eq!(w.since, ["\u{2192} 09:04", "30s"], "{w:?}");
    // The time has come, and long since: the cause alone.
    for late in [120, 121, 3600] {
        let w = words(&s, now + Duration::from_secs(late), 0);
        assert_eq!(w.phase, "can't reach the API", "{late}");
        assert!(w.since.is_empty(), "{late}: {w:?}");
        assert!(
            !w.sentence.contains("next try") && !w.sentence.contains("09:04"),
            "{late}: {}",
            w.sentence
        );
    }
    // A reset the wall names itself, whose countdown ended, stays.
    let usage = slot_of(
        TFacts {
            agent_seq: 1,
            agent: agent(AgentPhase::Wall {
                kind: TestWall::Usage,
                reset: Some("19:30".into()),
                until: Some(now + Duration::from_secs(120)),
            }),
            ..TFacts::default()
        },
        now,
    );
    let w = words(&usage, now + Duration::from_secs(3600), 0);
    assert_eq!(w.since, ["\u{2192} 19:30"], "{w:?}");
    assert!(w.sentence.contains("resets 19:30"), "{}", w.sentence);
}

#[test]
fn durations_read_like_the_mock() {
    assert_eq!(fmt_dur(Duration::from_secs(12)), "12s");
    assert_eq!(fmt_dur(Duration::from_secs(192)), "3m12s");
    assert_eq!(fmt_dur(Duration::from_secs(7_500)), "2h05m");
    assert_eq!(fmt_dur(Duration::from_secs(165_600)), "1d 22h");
    assert_eq!(short_sid("s-1e918c4662a1b7b8bd43"), "s-1e91");
}

/// The repaint term is 0 on a quiet view and nonzero the moment a rim, a
/// row or a ripple exists; a ripple steps nine times and then stops.
#[test]
fn window_view_fp_is_zero_when_quiet_and_the_ripple_steps_nine_times() {
    let now = Instant::now();
    let mut v = View::default();
    for i in 0..1000u64 {
        let t = now + Duration::from_millis(i);
        assert_eq!(v.fp(t), 0);
        assert!(
            v.rim_glow(t, true, || unreachable!("no tones on a quiet frame"))
                .is_none()
        );
        assert!(v.ripple_deadline(t).is_none());
    }
    assert!(!v.show(Level::Quiet, Rim::None, None, now), "nothing moved");
    v.rim = Rim::Drive;
    v.seed = 3;
    let steady = v.fp(now);
    assert_ne!(steady, 0);
    assert_eq!(
        v.fp(now + Duration::from_secs(60)),
        steady,
        "no clock in a steady rim"
    );
    v.ripple_at = Some(now);
    let mut seen = std::collections::BTreeSet::new();
    let mut t = now;
    while t < now + RIPPLE {
        seen.insert(v.fp(t));
        t += Duration::from_millis(1);
    }
    assert_eq!(seen.len(), 9, "nine ripple frames");
    assert_eq!(v.ripple_step(now + RIPPLE), None);
    assert!(v.ripple_deadline(now).is_some());
}

/// A stalled link with no date (a bridge still dialing since it attached)
/// prints the glyph alone, never `~ 0s`, and the sentence names no figure; a
/// dated stall still prints its seconds.
#[test]
fn adv6_a_stall_of_unknown_age_prints_no_figure() {
    let now = Instant::now();
    let mut s = TSlot::new(now);
    s.absorb(
        TFacts {
            link: Link::Stalled { age_ms: None },
            ..TFacts::default()
        },
        now,
    );
    let w = words(&s, now, 0);
    assert_ne!(w.fabric, "~ 0s", "sentence=`{}`", w.sentence);
    assert_eq!(w.fabric, "~");
    assert!(!w.sentence.contains("0 seconds"), "{}", w.sentence);
    assert!(w.sentence.ends_with("bridge stalled"), "{}", w.sentence);
    s.absorb(
        TFacts {
            link: Link::Stalled {
                age_ms: Some(7_400),
            },
            ..TFacts::default()
        },
        now,
    );
    let w = words(&s, now, 0);
    assert_eq!(w.fabric, "~ 7s");
    assert!(
        w.sentence.ends_with("bridge stalled 7 seconds"),
        "{}",
        w.sentence
    );
}

/// A hold the slot first sees at its mint is not dated from the mint:
/// nothing in a hold says when it began, so it is state with no figure, and
/// when it lifts the story says `resumed N ago` without a length. A hold
/// that begins after the baseline is dated and told in full.
#[test]
fn adv7_a_hold_seen_at_first_sight_has_no_made_up_duration() {
    let now = Instant::now();
    let mut s = TSlot::new(now);
    let held = |reason: &str| {
        Some(HoldFact {
            reason: reason.into(),
            fleet: false,
        })
    };
    let later = now + Duration::from_secs(3600);
    s.absorb(
        TFacts {
            hold: held("pause"),
            ..TFacts::default()
        },
        later,
    );
    let w = words(&s, later, 0);
    assert!(
        !w.since.iter().any(|c| c == "0s"),
        "the hold's age is unknown, the band says: {} / since={:?}",
        w.fit::<TestHost>(120),
        w.since
    );
    assert!(w.since.is_empty(), "{:?}", w.since);
    assert_eq!(w.hand, "\u{2298} hold pause");
    assert_eq!(s.level(0), Level::Hold);
    assert_eq!(s.story_seq, 0, "standing at the mint: state, not news");
    // Lifted 5 s later: the story is the resume alone.
    let lifted = later + Duration::from_secs(5);
    s.absorb(TFacts::default(), lifted);
    let at = lifted + Duration::from_secs(3);
    let w = words(&s, at, 0);
    assert_eq!(s.level(0), Level::Story);
    assert_eq!(
        w.since,
        vec!["since 3s".to_string(), "resumed 3s ago".to_string()]
    );
    assert!(!w.sentence.contains("held"), "{}", w.sentence);
    // A hold that begins after the baseline is dated and told in full.
    let began = at + Duration::from_secs(10);
    s.absorb(
        TFacts {
            hold: held("review"),
            ..TFacts::default()
        },
        began,
    );
    let w = words(&s, began + Duration::from_secs(4), 0);
    assert_eq!(w.since, vec!["4s".to_string()]);
    let end = began + Duration::from_secs(20);
    s.absorb(TFacts::default(), end);
    let w = words(&s, end + Duration::from_secs(1), 0);
    assert!(
        w.since.iter().any(|c| c == "held 20s, resumed 1s ago"),
        "{:?}",
        w.since
    );
}

/// A wall the harness answers is a record, not a stop: it stands at `Note`
/// (no rim, no chip, no warn tone), the row still there with its reset, its
/// countdown and words saying the harness resumes it. Negative controls:
/// nobody answering keeps the stop; a prompt or a person's hand outranks it.
#[test]
fn a_wall_the_harness_answers_is_a_note_and_a_wall_nobody_answers_is_a_stop() {
    let now = Instant::now();
    let wall = |answer: WallAnswer, reset: Option<&str>, left: Option<u64>| {
        slot_of(
            TFacts {
                agent_seq: 1,
                agent: agent(AgentPhase::Wall {
                    kind: TestWall::Usage,
                    reset: reset.map(str::to_string),
                    until: left.map(|s| now + Duration::from_secs(s)),
                }),
                wall_answer: answer,
                ..TFacts::default()
            },
            now,
        )
    };
    // The harness resumes it: a note. The row is there, the chrome is not.
    let handled = wall(WallAnswer::Harness, Some("4pm"), Some(13_260));
    // The wall arriving is a story point (a dot on the chip until the human
    // looks: "something happened while you were away", self-clearing); once
    // it has been looked at, what stands is the note.
    assert_eq!(handled.level(0), Level::Story);
    let looked = handled.story_seq;
    assert_eq!(handled.level(looked), Level::Note);
    assert_eq!(handled.shown_level(looked), Level::Note);
    assert_eq!(handled.level(looked).rim(), Rim::None);
    assert_eq!(handled.chip(looked), ChipLevel::Off);
    assert!(handled.level(looked).shows_row(), "the row is the record");
    assert_eq!(handled.tone(now, looked), Tone::Info, "no warn tone");
    assert!(
        !handled.calm(),
        "the row does not fold on a keystroke while the wall stands"
    );
    let w = words(&handled, now, looked);
    assert_eq!(w.phase, "limited");
    assert_eq!(
        w.since,
        ["\u{2192} 4pm", "3h41m"],
        "the reset and the time to it, as before"
    );
    assert!(w.sentence.contains("resumes by itself"), "{}", w.sentence);
    // Nobody answers it: the stop, as ever.
    let unanswered = wall(WallAnswer::Nobody, Some("4pm"), Some(13_260));
    assert_eq!(unanswered.level(looked), Level::Limited);
    assert_eq!(unanswered.level(looked).rim(), Rim::Stop { hold: false });
    assert_eq!(unanswered.chip(looked), ChipLevel::Stop(StopCause::Wall));
    assert_eq!(unanswered.tone(now, looked), Tone::Warn);
    assert!(
        !words(&unanswered, now, looked)
            .sentence
            .contains("resumes by itself")
    );
    // A wall the harness resumes slowly and a person can lift sooner: still a
    // note, with the one action in the row and in the sentence.
    let asks = wall(WallAnswer::HarnessAsks("/model to switch"), None, None);
    assert_eq!(asks.level(looked), Level::Note);
    assert_eq!(asks.chip(looked), ChipLevel::Off);
    let w = words(&asks, now, looked);
    assert_eq!(w.since, ["\u{2192} /model to switch"]);
    assert!(
        w.sentence
            .contains("resumes by itself, or sooner: /model to switch"),
        "{}",
        w.sentence
    );
    // Control: a prompt and a person's keyboard still outrank the note.
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
    let driven_wall = slot_of(
        TFacts {
            agent_seq: 1,
            agent: agent(AgentPhase::Wall {
                kind: TestWall::Usage,
                reset: None,
                until: None,
            }),
            wall_answer: WallAnswer::Harness,
            ..driven_facts()
        },
        now,
    );
    assert_eq!(driven_wall.level(looked), Level::Driven);
    // The answer moving (a claim lapses) is a change the view must repaint.
    let mut s = wall(WallAnswer::Harness, Some("4pm"), None);
    let mut facts = TFacts {
        agent_seq: 1,
        agent: agent(AgentPhase::Wall {
            kind: TestWall::Usage,
            reset: Some("4pm".to_string()),
            until: None,
        }),
        wall_answer: WallAnswer::Nobody,
        ..TFacts::default()
    };
    assert!(s.absorb(facts.clone(), now), "the answer moved");
    assert_eq!(s.level(looked), Level::Limited);
    facts.agent_seq = 2;
    assert!(!s.absorb(facts, now), "nothing moved");
}

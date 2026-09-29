// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The presence core proves itself without a host: a [`TestHost`] with a
//! two-kind wall and a plain stall drives the level ranking, the fold law,
//! the width law, the sanitizer, the painter and the idle view (design
//! ruling 348), and — moved from the host in round 31 (ruling 353) — the
//! row's words against the approved mock, elision, the severity table, the
//! story and its watermark, the tone, the durations and the ripple.

use crate::ink::{BandInks, InkedCell};
use crate::presence::{
    AgentPhase, AgentReading, ChipLevel, Facts, Hand, HoldFact, Host, Level, Link, MailFacts,
    MailLast, PresenceTones, RIPPLE, Rim, Slot, SlotKind, StopCause, Tone, TurnFact, View, Words,
    fmt_dur, paint_row, prints_seconds, prompt_band_word, sanitize_token, short_sid, stall_words,
    text_cols, words, words_step,
};
use crate::{Duration, Instant};

/// A test host: its walls are a usage window and a full context, its stall
/// is `(since, stopped, survived)`, its harness is `harness@1`, and its
/// percent codec spells a space `%20`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TestHost {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TestWall {
    Usage,
    Context,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TestStall {
    since: Instant,
    stopped: bool,
    survived: bool,
}

impl Host for TestHost {
    type Wall = TestWall;
    type Stall = TestStall;

    fn wall_wire(kind: TestWall) -> &'static str {
        match kind {
            TestWall::Usage => "wall:usage",
            TestWall::Context => "wall:context",
        }
    }

    fn wall_band(kind: TestWall) -> &'static str {
        match kind {
            TestWall::Usage => "limited",
            TestWall::Context => "context full",
        }
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

    fn is_aterms_holder(h: &str) -> bool {
        h == "harness@1"
    }

    fn pct_encode(s: &str) -> String {
        s.replace(' ', "%20")
    }

    fn pct_decode(s: &str) -> String {
        s.replace("%20", " ")
    }
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

/// THE FOLD LAW's model half: a hold that lifts leaves a story, which is not
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
            },
            ..TFacts::default()
        },
        now,
    );
    own.absorb(
        TFacts {
            hand: Hand::DrivenLease {
                holder: "harness@1".into(),
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

/// NO JARGON ON THE ROW (ruling 366; day nine, D2): a phase the reader
/// could not name prints no `unknown` and no age for it, and a session with
/// no mail prints no `✉0` — the slot is gone, not zero. Controls: the same
/// hand with a named phase prints the phase and its age, and one unread
/// mail prints `✉1`; a pending or queued count alone still shows the slot.
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
    let row = w.fit(120);
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
    let row = busy.fit(120);
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

/// The width law: rtt, since, role, ctx, then the mail's and the hand's
/// details shed, so phase, hand and mail survive to 24 columns; below that
/// the three are CUT, all on the row down to 13 columns; nothing is ever
/// wider than asked.
#[test]
fn elision_keeps_phase_hand_and_mail_to_24_and_all_three_to_13() {
    let now = Instant::now();
    let w = words(&driven(now), now + Duration::from_secs(192), 0);
    assert_eq!(
        w.fit(120),
        "worker:claude-satcomp  busy 3m12s  \u{25c2} manager \u{00b7} turn 41  \u{2709}2 task\u{2190}\u{2713}manager  ctx 41% left  \u{27df} 12ms"
    );
    assert!(!w.fit(90).contains('\u{27df}'), "rtt first");
    assert!(!w.fit(83).contains("3m12s"), "then since");
    assert!(!w.fit(75).contains("worker"), "then role");
    assert!(!w.fit(50).contains("ctx"), "then ctx");
    let n24 = w.fit(24);
    assert_eq!(n24, "busy  \u{25c2} turn 41  \u{2709}2");
    for cols in 13..=24 {
        let kinds: Vec<SlotKind> = w.pieces(cols).iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            [SlotKind::Phase, SlotKind::Hand, SlotKind::Mail],
            "{cols}: {}",
            w.fit(cols)
        );
    }
    for cols in 0..=200 {
        assert!(
            w.fit(cols).chars().count() <= cols,
            "{cols}: {}",
            w.fit(cols)
        );
    }
}

/// No free-text value may carry the row's glyphs, a bidi override or a
/// control byte, and the cap cuts with `…`.
#[test]
fn sanitize_refuses_band_glyphs_bidi_and_control_characters() {
    let s = sanitize_token(
        "\u{2298} hold pause \u{00b7}fleet\u{1f512}  \u{25c2} evil\u{202e}gnp\u{7}\t\nname\u{2066}",
        64,
    );
    assert_eq!(s, "hold pause fleet evilgnp name");
    assert_eq!(sanitize_token(&"x".repeat(200), 8), "xxxxxxx\u{2026}");
    for c in [
        '\u{200e}', '\u{200f}', '\u{202a}', '\u{2069}', '\u{1b}', '~', '\u{2192}',
    ] {
        assert_eq!(sanitize_token(&format!("a{c}b"), 8), "ab", "{c:?}");
    }
    // Every free-text slot goes through it.
    let now = Instant::now();
    let w = words(
        &slot_of(
            TFacts {
                role: Some("\u{25c2} fake hand".into()),
                hand: Hand::DrivenLease {
                    holder: "\u{2709}9 boss".into(),
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
            holder: "a b".into()
        }
        .wire::<TestHost>(),
        "lease:a%20b"
    );
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
        let row: Vec<InkedCell> = paint_row(&w, cols, &c, &p);
        assert_eq!(row.len(), cols);
        let text: String = row.iter().map(|k| k.ch).collect();
        assert_eq!(text.trim(), w.fit(text_cols(cols)), "{cols}");
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
        }
    }
    let row = paint_row(&w, 120, &c, &p);
    let hold = row.iter().find(|k| k.ch == '\u{2298}').expect("the hold");
    assert_eq!(hold.fg, p.stop);
    assert!(hold.bold);
}

/// THE IDLE LAW: a quiet view's fingerprint is exactly 0 at every instant,
/// it draws no rim, and a shown row, a rim or a ripple makes it nonzero.
#[test]
fn fp_is_zero_on_a_quiet_view() {
    let now = Instant::now();
    let mut v = View::default();
    for ms in 0..1000u64 {
        let t = now + Duration::from_millis(ms);
        assert_eq!(v.fp(t), 0);
        assert!(
            v.rim_glow(t, true, || unreachable!("no tones on a quiet frame"))
                .is_none()
        );
        assert!(v.ripple_deadline(t).is_none());
    }
    assert!(!v.show(Level::Quiet, Rim::None, None, now), "nothing moved");
    assert_eq!(v.fp(now), 0);
    assert!(v.show(Level::Driven, Rim::Drive, None, now));
    assert_ne!(v.fp(now), 0);
    v.ripple_at = Some(now);
    assert_eq!(v.ripple_step(now + Duration::from_millis(299)), Some(8));
    assert_eq!(v.ripple_step(now + crate::presence::RIPPLE), None);
}

/// The phase slot for a published input stall: `frozen <dur> · not reading
/// input` and its spoken remedy, the survivor's `end it with signal kill`,
/// and `stopped <dur> · input queued` for a stopped job. The duration counts
/// from the oldest unread byte. (Moved from the host's `input_stall`, where
/// a seam test still drives a real published stall through the row.)
#[test]
fn the_stall_says_frozen_or_stopped_with_the_age_and_the_remedy() {
    let now = Instant::now();
    assert_eq!(
        stall_words(now, false, false, now + Duration::from_secs(9660)),
        (
            "frozen".to_string(),
            vec!["2h41m".to_string(), "not reading input".to_string()],
            "frozen, not reading input for 2h41m; restart it".to_string(),
        )
    );
    assert_eq!(
        stall_words(now, false, true, now + Duration::from_secs(9660)),
        (
            "frozen".to_string(),
            vec!["2h41m".to_string(), "survived its restart".to_string()],
            "frozen, still running after its restart signal; end it with signal kill".to_string(),
        )
    );
    assert_eq!(
        stall_words(now, true, false, now + Duration::from_secs(12)),
        (
            "stopped".to_string(),
            vec!["12s".to_string(), "input queued".to_string()],
            "stopped with input queued for 12s; resume it".to_string(),
        )
    );
}

/// Which `since` clauses print SECONDS (and so tick every second): a token
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

// ---- moved from the host (round 31, ruling 351 (2)) -------------------------
// `aterm-gui/src/presence.rs`'s pure `Slot`/`words` tests, bodies verbatim
// over this file's seam: the usage wall is `TestWall::Usage`, the input stall
// a `TestStall`, the slot and facts `TSlot`/`TFacts`.

/// Display width in cells: every glyph the row uses is one cell wide.
fn width(s: &str) -> usize {
    s.chars().count()
}

/// THE MOCK'S FIRST BAND, verbatim: role, phase since, hand, mail with the
/// trust glyph BEFORE the sender, ctx, fabric.
#[test]
fn the_six_slots_read_like_the_approved_mock() {
    let now = Instant::now();
    let s = driven(now);
    let w = words(&s, now + Duration::from_secs(192), 0);
    assert_eq!(
        w.fit(120),
        "worker:claude-satcomp  busy 3m12s  \u{25c2} manager \u{00b7} turn 41  \u{2709}2 task\u{2190}\u{2713}manager  ctx 41% left  \u{27df} 12ms"
    );
    assert_eq!(
        w.sentence,
        "driven by manager, turn 41, busy, 2 unread, task from manager, agent, context 41 percent left"
    );
    assert_eq!(w.tone, Tone::Info);
    assert_eq!(s.level(0), Level::Driven);
    assert_eq!(s.level(0).rim(), Rim::Drive);
}

/// Elision right→left: rtt, then since, then role, then ctx; hand, phase
/// and mail survive to 24 columns.
#[test]
fn elision_sheds_from_the_right_and_keeps_hand_phase_mail_to_24_columns() {
    let now = Instant::now();
    let s = driven(now);
    let w = words(&s, now + Duration::from_secs(192), 0);
    let at = |cols| w.fit(cols);
    assert_eq!(width(&at(120)), 94, "the whole line: {}", at(120));
    assert!(!at(90).contains("\u{27df}"), "rtt goes first: {}", at(90));
    assert!(at(90).contains("3m12s"));
    assert!(!at(83).contains("3m12s"), "then since: {}", at(83));
    assert!(at(83).contains("worker:claude-satcomp"));
    assert!(!at(75).contains("worker:claude"), "then role: {}", at(75));
    assert!(at(75).contains("ctx 41% left"));
    assert!(!at(50).contains("ctx"), "then ctx: {}", at(50));
    assert!(at(50).contains("task\u{2190}\u{2713}manager"));
    assert!(!at(40).contains("task"), "then the mail detail: {}", at(40));
    assert!(at(40).contains("\u{25c2} manager \u{00b7} turn 41"));
    let narrow = at(28);
    assert!(
        !narrow.contains("manager"),
        "then the hand's holder: {narrow}"
    );
    let n24 = at(24);
    assert!(n24.contains("busy"), "{n24}");
    assert!(
        n24.contains("\u{25c2} turn 41"),
        "the hand's short form: {n24}"
    );
    assert!(n24.contains("\u{2709}2"), "the mail's short form: {n24}");
    assert!(!n24.contains("manager"), "{n24}");
    assert!(width(&n24) <= 24, "{n24}");
    for cols in [24usize, 60, 120] {
        assert!(width(&at(cols)) <= cols, "{cols}: {}", at(cols));
    }
}

/// RULING 270, "ONE PLACE TELLS IT": an attention whose only owner is
/// the agent upgrade is still a FACT — `level=attention`, `why=escalation`,
/// the tab chip's wait mark and the rim all keep it, so the stall shows at
/// the top after its band row folds — but the presence line's phase slot
/// does not repeat words the message band already says. NEGATIVE CONTROL:
/// the same text from any other owner reads on the line.
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
    // Round 18 (D1): the window's own chrome shows nothing for it — no
    // rim and no presence row, since the line has no words to say.
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
    // NEGATIVE CONTROL: another owner's attention is the line's phase.
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

/// AN ATTENTION TOLD ELSEWHERE NEVER TAKES THE ROW'S WORDS OR COLOUR
/// (ruling 280): with a Claude upgrade stall standing (its row and record
/// are the band's) and a story since the watermark, the row is shown at
/// the story's level — and its words are the story's summary, its tone
/// not Warn. Before 280 the row was shown but `words` and `tone` read the
/// full level: no summary, painted Warn — a colour with no words.
/// NEGATIVE CONTROL: the same story beside another owner's attention is
/// that attention's words, in Warn.
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
/// rim follows the level exactly — a colour with no words is unreachable.
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

/// THE INCIDENT'S BAND (2026-09-24): an approval box on the screen, the
/// supervisor's "answer this box" as typed attention, a supervisor's turn
/// on the hand — and a published input stall. The slot stands at
/// `Limited` (the stop rim), is not calm (a keystroke does not fold it),
/// and its phase reads `frozen` with the stall's age and why, ahead of the
/// attention text. A stopped job reads `stopped` and says to resume it.
/// NEGATIVE CONTROL: the same facts with the stall cleared are the box's
/// attention again, at `Attention`.
#[test]
fn a_stall_is_limited_not_calm_and_reads_frozen_over_typed_attention() {
    let now = Instant::now();
    let fact = TestStall {
        since: now,
        stopped: false,
        survived: false,
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
        w.fit(120)
            .contains("frozen 2m03s \u{00b7} not reading input"),
        "{}",
        w.fit(120)
    );
    assert!(!w.fit(120).contains("answer this box"), "{}", w.fit(120));
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
    // NEGATIVE CONTROL: the stall clears — the box's attention is back.
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
    // The longest stop survives elision: at 40 columns the counts go first.
    let narrow = w.fit(44);
    assert!(narrow.contains("held 1m00s"), "{narrow}");
    assert!(s.calm());
    assert_eq!(s.level(s.story_seq), Level::Quiet);
    assert!(!s.level(s.story_seq).shows_row());
}

/// D10 (2026-09-24): the story an agent told while nobody looked closes
/// when the agent LEAVES — a shell does not stand at `level=story` for a
/// run that is over, least of all in a headless instance where no person
/// ever acts to read it. NEGATIVE CONTROL: the same story with the agent
/// still there stands, and a new point after the exit is news again.
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

/// No command text, body, title or limit message reaches the words: a
/// hostile role/holder/reason is sanitized to a printable token.
#[test]
fn free_text_is_sanitized_and_the_limit_message_never_appears() {
    let now = Instant::now();
    let mut s = TSlot::new(now);
    s.absorb(
        TFacts {
            role: Some("evil\u{202e}role\nwith\tcontrol".into()),
            hand: Hand::DrivenLease {
                holder: "h\u{7f}older".into(),
            },
            agent_seq: 1,
            agent: Some(AgentReading {
                phase: AgentPhase::Wall {
                    kind: TestWall::Usage,
                    reset: Some("7:30pm".into()),
                    until: None,
                },
                context_pct: None,
            }),
            ..TFacts::default()
        },
        now,
    );
    let w = words(&s, now, 0);
    assert_eq!(w.role, "evilrole with control");
    assert_eq!(w.hand, "\u{25c2} holder");
    assert_eq!(w.phase, "limited");
    assert_eq!(w.since, vec!["\u{2192} 7:30pm".to_string()], "{w:?}");
    assert_eq!(
        sanitize_token("You've reached your limit", 8),
        "You've \u{2026}"
    );
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
        assert_eq!(v.fp(now + Duration::from_millis(i)), 0);
    }
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

/// ADV-6 (model): a stalled link with no date behind it (a bridge still
/// dialing since it attached) has no age — the composer used to print
/// `~ 0s` and speak "bridge stalled 0 seconds". The glyph alone now, and
/// the sentence names no figure; a dated stall still prints its seconds.
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

/// ADV-7 (model): a hold the slot first sees at its mint used to be dated
/// from the mint. Nothing in `Hold` says when it began, so the model
/// cannot know: the first absorb is the baseline, the hold is state with
/// no figure, and when it lifts the story says `resumed N ago` without a
/// length it never measured. A hold that BEGINS after the baseline is
/// dated and told in full.
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
        w.fit(120),
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
    // A hold that begins AFTER the baseline is dated and told in full.
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

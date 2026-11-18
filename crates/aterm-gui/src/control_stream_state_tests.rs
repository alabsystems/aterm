// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `state` record and the session's current-state record
//! ([`crate::session_timeline::AgentPublication`]), driven through the same rig as the stream's
//! own tests: a real claim on a real connection, the shipping lanes and reader, and records
//! written by the wire library's codec. The screen reader's side is its publish arm
//! (`publish_agent`, `set_composer`), called as the status sweep calls it.

use alab_wire::record::Composer;
use aterm_messages::presence::ComposerState;
use aterm_phase::Program;
use aterm_slog::{Actor, Conf, Key, Kind, Op, Rec, Src};

use super::super::super::control_session::cmd_await;
use super::*;
use crate::session_timeline::{AgentStamp, SessionTimeline, StreamedState};

fn state(state: State) -> Body {
    Body::State {
        state,
        composer: None,
    }
}

fn state_with(state: State, composer: Composer) -> Body {
    Body::State {
        state,
        composer: Some(composer),
    }
}

/// Run `read` on `sid`'s timeline.
fn timeline<R>(rig: &Rig, sid: &str, read: impl FnOnce(&mut SessionTimeline) -> R) -> R {
    let handle = rig.service.handle(sid);
    let mut timeline = handle
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    read(&mut timeline)
}

/// The screen reader publishes `word` for `sid`, as its sweep does: its own reading of a
/// Claude Code screen, or of a screen that shows no agent for `-`.
fn screen(rig: &Rig, sid: &str, word: &'static str, detail: Option<&str>) -> bool {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    timeline(rig, sid, |timeline| {
        timeline.publish_agent(
            word,
            detail.map(str::to_string),
            None,
            (word != "-").then_some(Program::Claude),
            AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq },
                fp: seq,
            },
        )
    })
}

/// The published `(word, detail, rev)` of `sid`.
fn published(rig: &Rig, sid: &str) -> (&'static str, Option<String>, u64) {
    timeline(rig, sid, |timeline| {
        let agent = timeline.agent();
        (agent.word, agent.detail.clone(), agent.rev)
    })
}

fn composer(rig: &Rig, sid: &str) -> &'static str {
    timeline(rig, sid, |timeline| timeline.agent().composer.word())
}

/// Every live record of `sid`'s log.
fn records(rig: &Rig, sid: &str) -> Vec<Rec> {
    timeline(rig, sid, |timeline| {
        timeline.slog().log().window().cloned().collect()
    })
}

/// The `mark agent` records of `sid`'s log, oldest first.
fn agent_marks(rig: &Rig, sid: &str) -> Vec<Rec> {
    records(rig, sid)
        .into_iter()
        .filter(|rec| rec.k == Kind::Agent && rec.op == Op::Mark)
        .collect()
}

/// `await agent <rest>` on `sid`, on this thread.
fn await_agent(rig: &Rig, sid: &str, rest: &str) -> String {
    let handle = rig.service.handle(sid);
    cmd_await(
        &handle.term,
        &rig.service.store,
        handle.local_id,
        &handle.ctx,
        &format!("agent {rest}"),
    )
}

/// What `status` and `log` show of `sid` that does not name where it was read from: the
/// `program=`, `agent=`, `agent_detail=` and `agent_rev=` words, `composer=`, the standing
/// needs, the `agent-change` rows' words and revisions, and of each `log kinds=agent` line its
/// operation with its `word=`, `agent=` and `out=`. The stamps that differ by source (`gen=`,
/// `fp=`, `hseq=`, `src=`, `conf=`, `by=`, `holder=`) are left out.
#[derive(Debug, PartialEq, Eq)]
struct Shown {
    status: Vec<String>,
    composer: &'static str,
    needs: Vec<&'static str>,
    rows: Vec<Vec<String>>,
    log: Vec<Vec<String>>,
}

fn shown(rig: &Rig, sid: &str) -> Shown {
    let handle = rig.service.handle(sid);
    let kept = |text: &str, keys: &[&str]| -> Vec<String> {
        text.split(' ')
            .filter(|word| keys.iter().any(|key| word.starts_with(key)))
            .map(str::to_string)
            .collect()
    };
    let (status, composer, needs, rows) = timeline(rig, sid, |timeline| {
        let agent = timeline.agent();
        (
            kept(
                &agent.wire_fields(),
                &["program=", "agent=", "agent_detail=", "agent_rev="],
            ),
            agent.composer.word(),
            timeline
                .slog()
                .log()
                .reader_state()
                .needs
                .iter()
                .flatten()
                .map(|need| need.word)
                .collect(),
            timeline
                .since(None)
                .filter(|row| row.kind == "agent-change")
                .map(|row| {
                    let mut words = row.payload.split(' ');
                    let word = words.next().unwrap_or_default().to_string();
                    let mut row = kept(&row.payload, &["rev="]);
                    row.insert(0, word);
                    row
                })
                .collect(),
        )
    });
    let log = super::super::super::control_session::cmd_log(&handle.ctx, "kinds=agent")
        .lines()
        .skip(1)
        .map(|line| {
            let mut row = vec![line.split(' ').nth(1).unwrap_or_default().to_string()];
            row.extend(kept(line, &["word=", "agent=", "out="]));
            row
        })
        .collect();
    Shown {
        status,
        composer,
        needs,
        rows,
        log,
    }
}

/// ONE SEQUENCE OF STATES, fed to one tab by the screen reader and to another as `state`
/// records, gives the same `status` words and the same log rows: the program, the agent word
/// and detail at every revision, the composer, the needs a box raises, the `agent-change` rows
/// and the agent run with its marks. Only the stamps that say where a row was read from differ.
/// NEGATIVE HALF: one more record to the stream's tab alone, and the two no longer read alike.
#[test]
fn a_recorded_stream_gives_the_status_and_log_rows_the_screen_reader_gave() {
    let rig = Rig::new(2);
    for sid in [ONE, TWO] {
        timeline(&rig, sid, |timeline| {
            timeline.note_foreground_group(7);
            timeline.set_program(7, Some("claude".to_string()));
        });
    }
    let mut stream = rig.claimant(TWO, "state");
    stream.open(TWO);
    let mut seq = 0;
    let mut record = |body: Body| {
        seq += 1;
        assert_eq!(stream.record(seq, body), (seq, None));
    };
    let read = |sid: &str, value: ComposerState| {
        timeline(&rig, sid, |timeline| timeline.set_composer(value));
    };
    let alike = |step: &str| assert_eq!(shown(&rig, ONE), shown(&rig, TWO), "{step}");

    screen(&rig, ONE, "busy", None);
    record(state(State::Working));
    alike("working");
    assert_eq!(published(&rig, TWO).0, "busy");

    // A permission box: the reader names it; under the stream the reader's own reading still
    // names the box's kind, and the record says the agent is asking.
    screen(&rig, ONE, "prompt", Some("bash"));
    screen(&rig, TWO, "prompt", Some("bash"));
    assert_eq!(published(&rig, TWO).0, "busy", "the reader decides no word");
    record(state(State::Asking));
    alike("asking, a permission box");
    assert_eq!(
        shown(&rig, TWO).needs,
        ["approval:bash"],
        "CONTROL: the box raised its need in both"
    );

    screen(&rig, ONE, "busy", None);
    record(state(State::Working));
    screen(&rig, TWO, "busy", None);
    alike("working again");

    read(ONE, ComposerState::Empty);
    screen(&rig, ONE, "idle", None);
    record(state_with(State::Idle, Composer::Empty));
    screen(&rig, TWO, "idle", None);
    alike("idle, an empty input box");
    assert_eq!(shown(&rig, TWO).composer, "empty");

    read(ONE, ComposerState::Draft);
    record(state_with(State::Idle, Composer::Draft));
    alike("a draft");
    assert_eq!(shown(&rig, TWO).composer, "draft");

    screen(&rig, ONE, "question", None);
    screen(&rig, TWO, "question", None);
    record(state_with(State::Asking, Composer::Draft));
    alike("asking, a question");
    assert_eq!(published(&rig, TWO).0, "question");

    screen(&rig, ONE, "idle", None);
    record(state_with(State::Idle, Composer::Draft));
    screen(&rig, TWO, "idle", None);
    alike("idle again");

    read(ONE, ComposerState::Unknown);
    screen(&rig, ONE, "-", None);
    record(state(State::Ended));
    alike("ended");
    let last = shown(&rig, TWO);
    assert_eq!(last.composer, "-");
    assert!(
        last.status.contains(&"agent=-".to_string()),
        "{:?}",
        last.status
    );
    assert_eq!(
        last.log.first().map(Vec::as_slice),
        Some(["open".to_string(), "agent=claude".to_string()].as_slice()),
        "CONTROL: the comparison read the agent run: {:?}",
        last.log
    );
    // The stamps the comparison left out do differ: a screen row names its screen, a stream
    // row its record.
    assert!(
        agent_marks(&rig, ONE)
            .iter()
            .all(|rec| rec.src == Src::Screen && rec.f.get_u(Key::Hseq).is_none())
    );
    assert!(
        agent_marks(&rig, TWO)
            .iter()
            .all(|rec| rec.src == Src::Harness && rec.f.get_u(Key::Hseq).is_some())
    );

    // NEGATIVE HALF: the comparison reads the record.
    record(state(State::Starting));
    assert_eq!(
        published(&rig, TWO),
        (
            "busy",
            Some("starting".to_string()),
            published(&rig, ONE).2 + 1
        )
    );
    assert_ne!(shown(&rig, ONE), shown(&rig, TWO));
}

/// `await agent` IS ANSWERED BY A `state` RECORD: with the screen reader stood down for the
/// tab, a wait for `busy` returns `OK agent busy rev=<n>` when the record says `working`.
/// NEGATIVE HALF: the screen reading `busy` meanwhile moves nothing and answers nothing.
#[test]
fn await_agent_answers_from_the_stream_with_the_reader_off() {
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    assert_eq!(stream.record(1, state(State::Idle)), (1, None));
    assert_eq!(published(&rig, ONE), ("idle", None, 1));

    // NEGATIVE HALF: the reader is off. Its `busy` is kept as its reading and decides nothing.
    assert!(!screen(&rig, ONE, "busy", None));
    assert_eq!(published(&rig, ONE), ("idle", None, 1));
    assert_eq!(await_agent(&rig, ONE, "busy timeout=50"), "OK timeout\n");

    let handle = rig.service.handle(ONE);
    let store = Arc::clone(&rig.service.store);
    let watchers = Arc::clone(&handle.ctx.watchers);
    let waiter = std::thread::spawn(move || {
        cmd_await(
            &handle.term,
            &store,
            handle.local_id,
            &handle.ctx,
            "agent busy since=1 timeout=60000",
        )
    });
    let deadline = Instant::now() + SETTLE;
    while watchers.count() == 0 {
        assert!(Instant::now() < deadline, "the await never parked");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!waiter.is_finished(), "nothing has moved the record yet");

    assert_eq!(stream.record(2, state(State::Working)), (2, None));
    assert_eq!(waiter.join().expect("the await"), "OK agent busy rev=2\n");
    // `starting` is `busy` too, with its detail.
    assert_eq!(stream.record(3, state(State::Starting)), (3, None));
    assert_eq!(
        published(&rig, ONE),
        ("busy", Some("starting".to_string()), 3)
    );
    assert_eq!(
        await_agent(&rig, ONE, "busy since=2 timeout=60000"),
        "OK agent busy rev=3\n"
    );
}

/// A CLAIM THAT HOLDS `state` AND NEVER SENDS IT DOES NOT BLANK ITS TAB: with a stream open
/// that applied `beat` and acknowledged a kind aterm does not apply, the agent word and the
/// composer are the screen reader's.
/// NEGATIVE HALF: from the first `state` record the reader decides neither, and when the claim
/// ends it decides both again.
#[test]
fn a_tab_without_state_is_still_fed_by_the_screen_reader() {
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "state");
    let id = stream.id();
    stream.open(ONE);
    assert_eq!(stream.record(1, Body::Beat), (1, None));
    assert_eq!(stream.record(2, goal()), (2, None));
    assert_eq!(
        rig.state(ONE).applied.iter().copied().collect::<Vec<_>>(),
        ["beat"],
        "`state` was never applied"
    );

    assert!(screen(&rig, ONE, "busy", None));
    timeline(&rig, ONE, |timeline| {
        timeline.set_composer(ComposerState::Draft)
    });
    assert_eq!(published(&rig, ONE), ("busy", None, 1));
    assert_eq!(composer(&rig, ONE), "draft");
    assert!(screen(&rig, ONE, "idle", None));
    assert_eq!(published(&rig, ONE), ("idle", None, 2));
    assert!(timeline(&rig, ONE, |timeline| timeline
        .agent()
        .stream
        .is_none()));
    assert_eq!(agent_marks(&rig, ONE).len(), 2);
    assert!(
        agent_marks(&rig, ONE)
            .iter()
            .all(|rec| rec.src == Src::Screen),
        "no kind is written twice, and none by the stream"
    );

    // NEGATIVE HALF: one `state` record, and the reader stands down for the word and the
    // composer. Its reading is kept.
    assert_eq!(stream.record(3, state(State::Working)), (3, None));
    assert_eq!(
        rig.state(ONE).applied.iter().copied().collect::<Vec<_>>(),
        ["beat", "state"]
    );
    assert_eq!(published(&rig, ONE), ("busy", None, 3));
    assert_eq!(
        composer(&rig, ONE),
        "-",
        "a record that names no composer reads `-`, not the reader's draft"
    );
    assert!(!screen(&rig, ONE, "idle", None));
    assert!(!screen(&rig, ONE, "question", None));
    timeline(&rig, ONE, |timeline| {
        timeline.set_composer(ComposerState::Empty)
    });
    assert_eq!(published(&rig, ONE), ("busy", None, 3));
    assert_eq!(composer(&rig, ONE), "-");
    let fed = timeline(&rig, ONE, |timeline| timeline.agent().stream.clone())
        .expect("the stream's state stands");
    assert_eq!((fed.state, fed.hseq), (StreamedState::Working, 3));
    assert_eq!(fed.epoch, crate::control_auth::process_instance_id());
    assert_eq!(
        fed.line,
        Record {
            seq: 3,
            ms: 0,
            body: state(State::Working)
        }
        .line()
        .expect("a record line"),
        "the applied line is kept as text"
    );

    // The claim is released: the reader feeds the tab again, from what it reads. (A closed
    // connection alone keeps a contract 4 claim, and the stand-down, through the re-exec grace.)
    assert!(crate::session_timeline::release_supervisor(
        &rig.service.handle(ONE).ctx,
        Some(id),
        crate::metrics::now_us(),
        Actor::Ctl,
    ));
    assert_eq!(published(&rig, ONE), ("question", None, 4));
    assert_eq!(composer(&rig, ONE), "empty");
    assert!(screen(&rig, ONE, "idle", None));
    assert_eq!(published(&rig, ONE).0, "idle");
}

/// `asking` AND `walled` NAME NO KIND, and no `ask` or `wall` record is applied yet: the kind
/// is the screen reader's. `asking` reads the screen's `question`, or its `prompt` with the
/// box, else `prompt`; `walled` reads the screen's `wall:<kind>`.
/// NEGATIVE HALF: the screen names a kind only for those two states: under `idle` and `working`
/// the same screen reading changes nothing.
#[test]
fn asking_and_walled_take_their_kind_from_the_screen_without_ask_or_wall_records() {
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "state,approvals,questions,walls");
    stream.open(ONE);
    let mut seq = 0;
    let mut record = |body: Body| {
        seq += 1;
        assert_eq!(stream.record(seq, body), (seq, None));
    };
    let reads = || {
        let (word, detail, _) = published(&rig, ONE);
        (word, detail)
    };

    // The screen reads `busy`: `asking` is `prompt`, the kind unnamed.
    record(state(State::Working));
    screen(&rig, ONE, "busy", None);
    record(state(State::Asking));
    assert_eq!(reads(), ("prompt", None));

    // The screen reads a question, then a permission box: the record follows the screen's kind.
    screen(&rig, ONE, "question", None);
    assert_eq!(reads(), ("question", None));
    screen(&rig, ONE, "prompt", Some("bash:not-read-only"));
    assert_eq!(reads(), ("prompt", Some("bash:not-read-only".to_string())));

    // NEGATIVE HALF: under `working` and `idle` the screen's box and wall name nothing.
    record(state(State::Working));
    assert_eq!(reads(), ("busy", None));
    screen(&rig, ONE, "wall:usage-session", Some("resets 5pm"));
    assert_eq!(reads(), ("busy", None));
    record(state(State::Idle));
    assert_eq!(reads(), ("idle", None));

    // `walled`: the screen's wall, kind and detail.
    record(state(State::Walled));
    assert_eq!(
        reads(),
        ("wall:usage-session", Some("resets 5pm".to_string()))
    );
    // The screen's box is no wall's kind, and its wall no ask's.
    screen(&rig, ONE, "prompt", Some("bash"));
    assert_eq!(reads(), ("wall", None));
    screen(&rig, ONE, "wall:usage-week", None);
    assert_eq!(reads(), ("wall:usage-week", None));
    record(state(State::Asking));
    assert_eq!(reads(), ("prompt", None));

    // No `ask` or `wall` record was applied: the screen was the only source of a kind.
    assert_eq!(
        rig.state(ONE).applied.iter().copied().collect::<Vec<_>>(),
        ["state"]
    );
    // Every one of those moves was written by the stream's arm, the screen-named kinds too.
    let marks = agent_marks(&rig, ONE);
    assert!(marks.iter().all(|rec| rec.src == Src::Harness), "{marks:?}");
    assert!(
        marks
            .iter()
            .any(|rec| rec.f.get_word(Key::Word) == Some("wall:usage-session"))
    );
}

/// A `walled` STATE WITH NO KIND ANYWHERE READS A BARE `wall`: `agent=wall`, a log row whose
/// word is `wall`, and `await agent wall` answers on it, as it still does on `wall:<kind>`.
/// NEGATIVE HALF: a wait for one kind of wall is not answered by the bare word, and a word
/// that only starts like `wall` is no wall.
#[test]
fn a_walled_state_with_no_kind_reads_wall_and_await_agent_wall_answers() {
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    assert_eq!(stream.record(1, state(State::Walled)), (1, None));
    assert_eq!(published(&rig, ONE), ("wall", None, 1));
    let status = timeline(&rig, ONE, |timeline| timeline.agent().wire_fields());
    assert!(status.contains(" agent=wall agent_detail=- "), "{status}");

    let marks = agent_marks(&rig, ONE);
    assert_eq!(marks.len(), 1);
    assert_eq!(
        marks[0].f.get_word(Key::Word),
        Some("wall"),
        "the row's word is `wall`, not `unknown`"
    );
    let row = timeline(&rig, ONE, |timeline| {
        timeline
            .since(None)
            .find(|row| row.kind == "agent-change")
            .map(|row| row.payload.to_string())
    });
    assert_eq!(row.as_deref(), Some("wall rev=1 hseq=1"));
    // It is treated as every wall is: a wall step that names no kind, and its need.
    let walls = |rig: &Rig| -> Vec<(Op, Option<&'static str>)> {
        records(rig, ONE)
            .into_iter()
            .filter(|rec| rec.k == Kind::Wall)
            .map(|rec| (rec.op, rec.f.get_word(Key::Wall)))
            .collect()
    };
    assert_eq!(
        walls(&rig),
        [(Op::Open, None)],
        "a wall step, no kind guessed"
    );
    assert_eq!(shown(&rig, ONE).needs, ["wall"], "and the need it raises");

    assert_eq!(
        await_agent(&rig, ONE, "wall timeout=60000"),
        "OK agent wall rev=1\n"
    );
    // NEGATIVE HALF.
    assert_eq!(
        await_agent(&rig, ONE, "wall:usage-session timeout=50"),
        "OK timeout\n"
    );
    assert_eq!(
        await_agent(&rig, ONE, "idle,busy timeout=50"),
        "OK timeout\n"
    );

    // The screen names the kind: `wall` still answers, and so does the kind.
    screen(&rig, ONE, "wall:usage-session", None);
    assert_eq!(published(&rig, ONE), ("wall:usage-session", None, 2));
    assert_eq!(
        walls(&rig),
        [
            (Op::Open, None),
            (Op::Close, None),
            (Op::Open, Some("usage-session"))
        ],
        "the wall of the named kind replaces the bare one"
    );
    assert_eq!(shown(&rig, ONE).needs, ["wall:usage-session"]);
    assert_eq!(
        await_agent(&rig, ONE, "wall timeout=60000"),
        "OK agent wall:usage-session rev=2\n"
    );
    assert_eq!(
        await_agent(&rig, ONE, "wall:usage-session timeout=60000"),
        "OK agent wall:usage-session rev=2\n"
    );
    assert_eq!(
        await_agent(&rig, ONE, "wall:usage-week timeout=50"),
        "OK timeout\n"
    );
    // NEGATIVE HALF: a state that is no wall leaves no wall step open and no wall's need.
    assert_eq!(stream.record(2, state(State::Working)), (2, None));
    assert_eq!(
        walls(&rig).iter().map(|(op, _)| *op).collect::<Vec<_>>(),
        [Op::Open, Op::Close, Op::Open, Op::Close],
        "the wall step closed"
    );
    assert!(shown(&rig, ONE).needs.is_empty());
}

/// THE ROW A `state` RECORD WRITES is `mark agent word= rev= hseq= holder=` by the tab's own
/// session, `src=harness conf=claim`, with the record's `seq` as `hseq` and no `gen=` or `fp=`;
/// the `agent-change` row prints `hseq=`. It neither reads nor spends the session's mark
/// budget: with three tokens left, three stream marks leave exactly three, and with none left
/// a record is still written.
/// NEGATIVE HALF: a write arm that spent a token a mark would leave fewer than three acts
/// admitted; with the budget at zero a `mark` claim by the same session is refused; and a
/// screen reader's row carries `gen=` and `fp=` and neither `hseq=` nor `holder=`.
#[test]
fn a_state_record_is_written_with_holder_src_harness_conf_claim_and_by_the_session() {
    let rig = Rig::new(2);
    let by = crate::session_log::sid_actor(ONE).expect("a minted session id");
    assert!(matches!(by, Actor::Sid(_)));
    let holder = format!("claude-code-alab@{}", running().pid);
    // The session log on a hand clock, so no token of the mark budget comes back.
    timeline(&rig, ONE, |timeline| {
        timeline
            .slog_mut()
            .set_clock(crate::session_log::Clock::reading(|| {
                aterm_slog::Stamp::new(1_792_000_000_000, 5_000)
            }));
    });
    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    assert_eq!(stream.record(1, Body::Beat), (1, None));

    // The session's whole mark budget on this clock, counted on the other tab, whose budget is
    // its own.
    let acts = |sid: &str, by: Actor, most: usize| {
        timeline(&rig, sid, |timeline| {
            (0..most)
                .take_while(|_| timeline.note_harness_act("approve", &holder, by).is_some())
                .count()
        })
    };
    timeline(&rig, TWO, |timeline| {
        timeline
            .slog_mut()
            .set_clock(crate::session_log::Clock::reading(|| {
                aterm_slog::Stamp::new(1_792_000_000_000, 5_000)
            }));
    });
    let other = crate::session_log::sid_actor(TWO).expect("a minted session id");
    let budget = acts(TWO, other, 10_000);
    const LEFT: usize = 3;
    assert!(
        (LEFT + 1..10_000).contains(&budget),
        "CONTROL: the budget is finite and holds more than {LEFT}: {budget}"
    );
    // Spend this session's budget down to exactly LEFT tokens.
    assert_eq!(acts(ONE, by, budget - LEFT), budget - LEFT);

    // The stream writes three marks by this same session with LEFT tokens in its budget.
    for (seq, body) in [
        (2, state(State::Working)),
        (3, state(State::Idle)),
        (4, state(State::Working)),
    ] {
        assert_eq!(stream.record(seq, body), (seq, None));
    }
    // NEGATIVE HALF of "nor spends": a write arm that spent a token for each mark would have
    // left none. Exactly LEFT acts are still admitted, and the next is refused.
    assert_eq!(
        acts(ONE, by, LEFT + 1),
        LEFT,
        "the stream's three marks spent none of the session's mark budget"
    );
    let refused = |rig: &Rig| {
        timeline(rig, ONE, |timeline| {
            timeline.note_harness_act("approve", &holder, by).is_none()
        })
    };
    assert!(
        refused(&rig),
        "NEGATIVE HALF: a `mark` claim is refused now"
    );
    // "Neither reads": with the budget spent, a record is still written.
    assert_eq!(stream.record(5, state(State::Idle)), (5, None));
    assert_eq!(
        agent_marks(&rig, ONE)
            .last()
            .and_then(|rec| rec.f.get_u(Key::Hseq)),
        Some(5),
        "a mark written with the session's mark budget at zero"
    );
    assert!(refused(&rig), "and the stream's rows gave it nothing back");
    let marks: Vec<Rec> = agent_marks(&rig, ONE).into_iter().take(3).collect();
    let rows: Vec<String> = timeline(&rig, ONE, |timeline| {
        timeline
            .since(None)
            .filter(|row| row.kind == "agent-change")
            .map(|row| row.payload.to_string())
            .take(3)
            .collect()
    });

    let read: Vec<_> = marks
        .iter()
        .map(|rec| {
            (
                rec.f.get_word(Key::Word),
                rec.f.get_u(Key::Rev),
                rec.f.get_u(Key::Hseq),
                rec.f.get_str(Key::Holder),
                (rec.src, rec.conf, rec.by),
                rec.f.get_str(Key::Gen).or(rec.f.get_str(Key::Fp)),
            )
        })
        .collect();
    let stamps = (Src::Harness, Conf::Claim, by);
    assert_eq!(
        read,
        [
            (
                Some("busy"),
                Some(1),
                Some(2),
                Some(holder.as_str()),
                stamps,
                None
            ),
            (
                Some("idle"),
                Some(2),
                Some(3),
                Some(holder.as_str()),
                stamps,
                None
            ),
            (
                Some("busy"),
                Some(3),
                Some(4),
                Some(holder.as_str()),
                stamps,
                None
            ),
        ]
    );
    let line = aterm_slog::encode(&marks[0], aterm_slog::Tier::Host).expect("a record line");
    for word in [
        " mark ",
        " agent ",
        "word=busy",
        "rev=1",
        "hseq=2",
        &format!("holder={holder}"),
        "src=harness",
        "conf=claim",
        &format!("by={ONE}"),
    ] {
        assert!(line.contains(word), "{word} in {line}");
    }
    assert_eq!(
        rows,
        [
            "busy rev=1 hseq=2",
            "idle rev=2 hseq=3",
            "busy rev=3 hseq=4"
        ]
    );

    // NEGATIVE HALF: the screen reader's row, in a tab no stream feeds.
    screen(&rig, TWO, "busy", None);
    let screen_marks = agent_marks(&rig, TWO);
    assert_eq!(screen_marks.len(), 1);
    let rec = &screen_marks[0];
    assert_eq!((rec.src, rec.conf), (Src::Screen, Conf::Strong));
    assert!(rec.f.get_str(Key::Gen).is_some() && rec.f.get_str(Key::Fp).is_some());
    assert_eq!(
        (rec.f.get_u(Key::Hseq), rec.f.get_str(Key::Holder)),
        (None, None)
    );
    let row = timeline(&rig, TWO, |timeline| {
        timeline
            .since(None)
            .find(|row| row.kind == "agent-change")
            .map(|row| row.payload.to_string())
    })
    .expect("the screen's row");
    assert!(
        row.starts_with("busy rev=1 gen=") && row.contains(" fp=") && !row.contains("hseq="),
        "{row}"
    );
}

/// A STREAM'S TAB WITH NO AGENT RUN GETS ONE FROM THE CLAIM'S REGISTRY ROW (D15): nothing read
/// the screen and no process name was resolved, and the first record the stream APPLIES, of
/// any kind (here a `beat`), opens an agent run for the agent the claim's row names. Two rows
/// name two agents: claude-code-alab leads no word, so its agent is read from its id
/// (`claude`); grok-alab's is `grok`.
/// NEGATIVE HALF: a claim with no stream, a record acknowledged and not applied, and a record
/// dropped for its scope each open no run; a second applied record opens no second run;
/// `ended` closes the run, and a `beat` after `ended` opens none.
#[test]
fn a_stream_tab_opens_its_agent_run_from_the_claims_row() {
    let rig = Rig::new(2);
    let runs = |rig: &Rig, sid: &str| -> Vec<Rec> {
        records(rig, sid)
            .into_iter()
            .filter(|rec| rec.k == Kind::Agent && rec.op != Op::Mark)
            .collect()
    };
    let row_of = |sid: &str| {
        let handle = rig.service.handle(sid);
        let meta = handle
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        meta.live_scoped(crate::metrics::now_us())
            .expect("the claim")
            .claim
            .row
    };
    // Two tabs, two rows, two agents.
    let mut claude = rig.claimant(ONE, "state");
    let mut grok = rig.dial("owner");
    assert_eq!(
        grok.request(&format!("claim {TWO} 4 state grok-alab")),
        "OK claim"
    );
    let row = row_of(ONE);
    assert_eq!(row.id, "claude-code-alab");
    assert_eq!(
        (row.program, aterm_phase::program_of(row.id)),
        (Program::Claude, Some(Program::Claude)),
        "CONTROL: a row that leads nothing names its id's agent"
    );
    let row = row_of(TWO);
    assert_eq!(row.id, "grok-alab");
    assert_eq!(
        row.program,
        Program::Grok,
        "CONTROL: the second row names another agent, so no constant is both"
    );

    for (sid, stream, agent) in [(ONE, &mut claude, "claude"), (TWO, &mut grok, "grok")] {
        let by = crate::session_log::sid_actor(sid).expect("a minted session id");
        // NEGATIVE HALF: the claim alone, with no stream and no record, opened nothing.
        assert!(runs(&rig, sid).is_empty(), "{sid}: {:?}", runs(&rig, sid));
        stream.open(sid);
        // NEGATIVE HALF: a record acknowledged and not applied, and one the grant drops.
        assert_eq!(stream.record(1, goal()), (1, None));
        assert_eq!(
            stream.record(2, title()),
            (2, None),
            "a title is acknowledged and applied by nothing"
        );
        assert_eq!(
            stream.record(
                3,
                Body::Wall {
                    kind: "usage".to_string(),
                    resets_ms: None,
                }
            ),
            (3, Some("scope".to_string())),
            "the grant holds no `walls`"
        );
        assert!(runs(&rig, sid).is_empty(), "{sid}: {:?}", runs(&rig, sid));
        assert!(timeline(&rig, sid, |timeline| {
            let agent = timeline.agent();
            agent.reader.is_none() && agent.program.is_none() && agent.word == "-"
        }));

        // The first applied record, a `beat`: the run opens for the row's agent.
        assert_eq!(stream.record(4, Body::Beat), (4, None));
        let opened = runs(&rig, sid);
        assert_eq!(opened.len(), 1, "{sid}: {opened:?}");
        let run = &opened[0];
        assert_eq!(run.op, Op::Open);
        assert_eq!(
            run.f.get_str(Key::Agent),
            Some(agent),
            "{sid}: the agent is the claim's row's"
        );
        assert_eq!((run.src, run.conf, run.by), (Src::Harness, Conf::Claim, by));
        assert_eq!(published(&rig, sid).0, "-", "a beat moves no agent word");
        // A second applied record, and a `state`, open no second run; the record's mark is on
        // the one the row opened.
        assert_eq!(stream.record(5, Body::Beat), (5, None));
        assert_eq!(stream.record(6, state(State::Working)), (6, None));
        assert_eq!(runs(&rig, sid).len(), 1);
        let marks = agent_marks(&rig, sid);
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].node, run.node, "the record's mark is on the run");
        assert!(timeline(&rig, sid, |timeline| {
            timeline.slog().log().is_open(run.node)
        }));

        // `ended` closes it, and a beat after `ended` opens none: the record said it is gone.
        assert_eq!(stream.record(7, state(State::Ended)), (7, None));
        assert!(!timeline(&rig, sid, |timeline| {
            timeline.slog().log().is_open(run.node)
        }));
        assert_eq!(stream.record(8, Body::Beat), (8, None));
        assert_eq!(
            runs(&rig, sid).iter().map(|rec| rec.op).collect::<Vec<_>>(),
            [Op::Open, Op::Close]
        );
        // A `state` that says the agent is back opens the next run, again from the row.
        assert_eq!(stream.record(9, state(State::Idle)), (9, None));
        let reopened = runs(&rig, sid);
        assert_eq!(reopened.len(), 3);
        assert_eq!(reopened[2].f.get_str(Key::Agent), Some(agent));
    }
}

/// A `state` THAT REPEATS THE PUBLISHED WORD STILL OPENS THE ROW'S RUN (D15): the screen reader
/// published `idle` for a tab whose reader names no agent, so no run is open; the stream's
/// `idle` moves no word and writes no mark, and the run opens all the same.
/// NEGATIVE HALF: before the record there is no run, and the record writes no `mark agent`.
#[test]
fn a_state_that_moves_no_word_still_opens_the_rows_run() {
    let rig = Rig::new(2);
    let runs = |rig: &Rig| -> Vec<Rec> {
        records(rig, ONE)
            .into_iter()
            .filter(|rec| rec.k == Kind::Agent && rec.op != Op::Mark)
            .collect()
    };
    // The screen's word with no reader named: the reader opens no run.
    timeline(&rig, ONE, |timeline| {
        timeline.publish_agent(
            "idle",
            None,
            None,
            None,
            AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        )
    });
    assert_eq!(published(&rig, ONE), ("idle", None, 1));
    let screen_marks = agent_marks(&rig, ONE).len();
    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    assert!(
        runs(&rig).is_empty(),
        "NEGATIVE HALF: no run before a record"
    );

    assert_eq!(stream.record(1, state(State::Idle)), (1, None));
    assert_eq!(
        published(&rig, ONE),
        ("idle", None, 1),
        "the word did not move"
    );
    assert_eq!(
        agent_marks(&rig, ONE).len(),
        screen_marks,
        "and no mark was written for it"
    );
    let opened = runs(&rig);
    assert_eq!(opened.len(), 1, "{opened:?}");
    assert_eq!(opened[0].f.get_str(Key::Agent), Some("claude"));
    assert_eq!(opened[0].src, Src::Harness);
}

/// THE STAND-DOWN ENDS WHEN THE CLAIM NO LONGER FEEDS `state`, though the claim's holder still
/// holds the tab and no `meta-change` is written: a renewal whose grant drops `state`, and a
/// renewal by another process under the same holder name. Each hands the agent word and the
/// composer back to the screen reader at once, from what it reads.
/// NEGATIVE HALF: a renewal in place, and one that widens the grant and keeps `state`, leave
/// the reader down; and the new claimant's own `state` record stands it down again.
#[test]
fn the_stand_down_ends_when_a_renewal_drops_state_or_changes_the_claimant() {
    // A lane for each stream this test opens: a stream whose claim moved to another
    // connection closes within its tick, and no open here waits on that.
    let rig = Rig::new(4);
    let fed = |sid: &str| timeline(&rig, sid, |timeline| timeline.agent().stream.is_some());
    let supervisor_rows = |sid: &str| {
        timeline(&rig, sid, |timeline| {
            timeline
                .since(None)
                .filter(|row| row.kind == "meta-change" && row.payload.contains("supervisor"))
                .count()
        })
    };

    // A renewal whose grant drops `state`.
    let mut stream = rig.claimant(ONE, "state");
    let id = stream.id();
    stream.open(ONE);
    assert_eq!(
        stream.record(1, state_with(State::Working, Composer::Draft)),
        (1, None)
    );
    assert!(!screen(&rig, ONE, "idle", None));
    timeline(&rig, ONE, |timeline| {
        timeline.set_composer(ComposerState::Empty)
    });
    assert_eq!(published(&rig, ONE), ("busy", None, 1));
    assert_eq!(composer(&rig, ONE), "draft");
    let rows = supervisor_rows(ONE);
    // NEGATIVE HALF: renewals that keep `state` keep the reader down.
    let handle = rig.service.handle(ONE);
    place(
        &handle,
        4,
        &[Duty::State],
        id,
        running(),
        "claude-code-alab",
    )
    .expect("in place");
    assert!(fed(ONE), "a renewal in place keeps the stand-down");
    place(
        &handle,
        4,
        &[Duty::State, Duty::Walls],
        id,
        running(),
        "claude-code-alab",
    )
    .expect("a wider grant");
    assert!(
        fed(ONE),
        "a renewal that keeps `state` keeps the stand-down"
    );
    assert_eq!(published(&rig, ONE), ("busy", None, 1));
    // The renewal without `state`, from the same process on another connection.
    let mut renewed = rig.dial("owner");
    assert_eq!(renewed.request(&format!("claim {ONE} 4 walls")), "OK claim");
    assert_eq!(
        supervisor_rows(ONE),
        rows,
        "CONTROL: a renewal by the same claimant writes no meta-change"
    );
    assert!(!fed(ONE), "the claim no longer holds `state`");
    assert_eq!(
        published(&rig, ONE),
        ("idle", None, 2),
        "the screen reader's kept reading is published"
    );
    assert_eq!(composer(&rig, ONE), "empty");
    assert!(screen(&rig, ONE, "busy", None), "and it decides again");
    // The grant drops the record from here on, and it stands nothing down.
    renewed.open(ONE);
    let acked = rig.state(ONE).acked;
    assert_eq!(
        renewed.record(acked + 1, state(State::Idle)),
        (acked + 1, Some("scope".to_string()))
    );
    assert!(!fed(ONE));
    assert_eq!(published(&rig, ONE).0, "busy");
    drop(stream);

    // A renewal by another process under the same holder name.
    let mut first = rig.claimant(TWO, "state");
    first.open(TWO);
    assert_eq!(first.record(1, state(State::Asking)), (1, None));
    assert_eq!(published(&rig, TWO), ("prompt", None, 1));
    assert!(!screen(&rig, TWO, "idle", None));
    let rows = supervisor_rows(TWO);
    let mut second = rig.dial("owner");
    assert_eq!(
        second.request(&format!("claim {TWO} 4 state other")),
        "OK claim"
    );
    assert_eq!(
        supervisor_rows(TWO),
        rows,
        "CONTROL: the same holder name renews, and writes no meta-change"
    );
    assert!(!fed(TWO), "the stream's state was another claimant's");
    assert_eq!(published(&rig, TWO), ("idle", None, 2));
    // NEGATIVE HALF: the new claimant's own `state` stands the reader down again.
    second.open(TWO);
    assert_eq!(second.record(1, state(State::Working)), (1, None));
    assert!(fed(TWO));
    assert_eq!(published(&rig, TWO), ("busy", None, 3));
    assert!(!screen(&rig, TWO, "question", None));
}

/// A CLAIM RELEASED WITH ITS `ttl=` ALREADY PAST ENDS THE STAND-DOWN: the release of a claim
/// that is no longer live writes no `meta-change`, and no lapse follows a claim that is gone,
/// so the release itself hands the tab back to the screen reader.
/// NEGATIVE HALF: until the claim is released the record stands, and the release wrote no
/// `meta-change` that could have ended it.
#[test]
fn a_stream_claim_released_past_its_ttl_ends_the_stand_down() {
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "state");
    let id = stream.id();
    stream.open(ONE);
    assert_eq!(stream.record(1, state(State::Working)), (1, None));
    assert!(!screen(&rig, ONE, "question", None));
    assert_eq!(published(&rig, ONE), ("busy", None, 1));
    let supervisor_rows = || {
        timeline(&rig, ONE, |timeline| {
            timeline
                .since(None)
                .filter(|row| row.kind == "meta-change" && row.payload.contains("supervisor"))
                .count()
        })
    };
    let rows = supervisor_rows();

    // The claim's `ttl=` passes. Nothing has released it yet: the record still stands.
    {
        let handle = rig.service.handle(ONE);
        let mut meta = handle
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        meta.supervisor.as_mut().expect("the claim").expires_us = Some(1);
        assert!(meta.live_claim(crate::metrics::now_us()).is_none());
    }
    assert!(
        timeline(&rig, ONE, |timeline| timeline.agent().stream.is_some()),
        "NEGATIVE HALF: an expired claim nobody released has moved nothing"
    );

    // Its connection ends and releases it, before any lapse ran.
    drop(stream);
    rig.ended(id);
    assert!(
        rig.service
            .handle(ONE)
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .supervisor
            .is_none(),
        "the claim is gone"
    );
    assert_eq!(
        supervisor_rows(),
        rows,
        "CONTROL: releasing a claim past its ttl writes no meta-change"
    );
    assert!(timeline(&rig, ONE, |timeline| timeline
        .agent()
        .stream
        .is_none()));
    assert_eq!(published(&rig, ONE), ("question", None, 2));
    assert!(screen(&rig, ONE, "idle", None), "the reader decides again");
}

/// `await agent stopped` IS A WAIT aterm TAKES, and it answers when the published word is
/// `stopped`; the word is logged as itself.
/// NEGATIVE HALF: a word outside the vocabulary is still a usage error, and `stopped` is not
/// answered by another word.
#[test]
fn await_agent_takes_stopped_and_answers_on_it() {
    let rig = Rig::new(2);
    assert!(screen(&rig, ONE, "busy", None));
    assert_eq!(
        await_agent(&rig, ONE, "stopped timeout=50"),
        "OK timeout\n",
        "the word is taken, and `busy` does not answer it"
    );
    let refused = await_agent(&rig, ONE, "stoppedx timeout=50");
    assert!(
        refused.starts_with("ERR usage"),
        "NEGATIVE HALF: a word outside the vocabulary: {refused}"
    );
    assert!(screen(&rig, ONE, "stopped", None));
    assert_eq!(
        await_agent(&rig, ONE, "stopped timeout=60000"),
        "OK agent stopped rev=2\n"
    );
    assert_eq!(
        await_agent(&rig, ONE, "idle,stopped since=1 timeout=60000"),
        "OK agent stopped rev=2\n"
    );
    assert_eq!(
        agent_marks(&rig, ONE)
            .last()
            .and_then(|rec| rec.f.get_word(Key::Word)),
        Some("stopped"),
        "the row's word is `stopped`, not `unknown`"
    );
}

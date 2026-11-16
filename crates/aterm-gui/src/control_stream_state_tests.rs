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

    // The claim ends with its connection: the reader feeds the tab again, from what it reads.
    drop(stream);
    rig.ended(id);
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
}

/// THE ROW A `state` RECORD WRITES is `mark agent word= rev= hseq= holder=` by the tab's own
/// session, `src=harness conf=claim`, with the record's `seq` as `hseq` and no `gen=` or `fp=`;
/// the `agent-change` row prints `hseq=`. It spends none of the session's mark budget: it is
/// written with that budget spent.
/// NEGATIVE HALF: at that moment a `mark` claim by the same session is refused, and a screen
/// reader's row carries `gen=` and `fp=` and neither `hseq=` nor `holder=`.
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

    // Spend the session's own mark budget to the last token.
    let spent = timeline(&rig, ONE, |timeline| {
        (0..10_000)
            .take_while(|_| timeline.note_harness_act("approve", &holder, by).is_some())
            .count()
    });
    assert!(
        (1..10_000).contains(&spent),
        "CONTROL: the budget was there and ran out: {spent}"
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

    for (seq, body) in [
        (2, state(State::Working)),
        (3, state(State::Idle)),
        (4, state(State::Working)),
    ] {
        assert_eq!(stream.record(seq, body), (seq, None));
    }
    assert!(refused(&rig), "and the stream's rows gave it nothing back");

    let marks = agent_marks(&rig, ONE);
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
    let rows: Vec<String> = timeline(&rig, ONE, |timeline| {
        timeline
            .since(None)
            .filter(|row| row.kind == "agent-change")
            .map(|row| row.payload.to_string())
            .collect()
    });
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

/// A STREAM'S TAB WITH NO AGENT RUN GETS ONE FROM THE CLAIM'S REGISTRY ROW: nothing read the
/// screen and no process name was resolved, and the first `state` record opens an agent run for
/// the agent the row names, with the record's mark on it. The fixture's row leads no word, so
/// its agent is read from its id.
/// NEGATIVE HALF: a stream that applies no `state` opens no run, and `ended` closes the run the
/// row opened.
#[test]
fn a_stream_tab_opens_its_agent_run_from_the_claims_row() {
    let rig = Rig::new(2);
    let by = crate::session_log::sid_actor(ONE).expect("a minted session id");
    let runs = |rig: &Rig| -> Vec<Rec> {
        records(rig, ONE)
            .into_iter()
            .filter(|rec| rec.k == Kind::Agent && rec.op != Op::Mark)
            .collect()
    };
    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    let row = {
        let handle = rig.service.handle(ONE);
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
    assert_eq!((row.id, row.program), ("claude-code-alab", Program::Claude));
    assert_eq!(
        aterm_phase::program_of(row.id),
        Some(Program::Claude),
        "CONTROL: the row's agent is its id's"
    );

    // NEGATIVE HALF: a stream that applied no `state` opened nothing.
    assert_eq!(stream.record(1, Body::Beat), (1, None));
    assert_eq!(stream.record(2, goal()), (2, None));
    assert!(runs(&rig).is_empty(), "{:?}", runs(&rig));
    assert!(timeline(&rig, ONE, |timeline| {
        let agent = timeline.agent();
        agent.reader.is_none() && agent.program.is_none()
    }));

    assert_eq!(stream.record(3, state(State::Working)), (3, None));
    let opened = runs(&rig);
    assert_eq!(opened.len(), 1, "{opened:?}");
    let run = &opened[0];
    assert_eq!(run.op, Op::Open);
    assert_eq!(run.f.get_str(Key::Agent), Some("claude"));
    assert_eq!((run.src, run.conf, run.by), (Src::Harness, Conf::Claim, by));
    let marks = agent_marks(&rig, ONE);
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].node, run.node, "the record's mark is on the run");
    assert!(timeline(&rig, ONE, |timeline| {
        timeline.slog().log().is_open(run.node)
    }));
    // A second record opens no second run.
    assert_eq!(stream.record(4, state(State::Idle)), (4, None));
    assert_eq!(runs(&rig).len(), 1);

    // `ended` closes it.
    assert_eq!(stream.record(5, state(State::Ended)), (5, None));
    assert!(!timeline(&rig, ONE, |timeline| {
        timeline.slog().log().is_open(run.node)
    }));
    assert_eq!(
        runs(&rig).iter().map(|rec| rec.op).collect::<Vec<_>>(),
        [Op::Open, Op::Close]
    );
}

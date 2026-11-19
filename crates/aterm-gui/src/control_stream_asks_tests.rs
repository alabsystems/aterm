// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `ask`, `asked` and `wall` records and aterm's progress watch (HX-15i1), driven through the
//! same rig as the stream's own tests: a real claim on a real connection, the shipping lanes and
//! reader, records written by the wire library's codec, and the rig's frozen awake clock, which
//! the stream lane's watchdog counts on. The screen reader's side is its publish arm
//! (`publish_agent`, through `screen`), called as the status sweep calls it. The claimant is the
//! shared stand-in and no harness state directory is at hand, so a restart is told and not
//! watched: nothing is ever signalled.

use alab_wire::record::{AskKind, By, Reply, Step as WireStep, TurnState};
use aterm_slog::{Actor, Conf, Key, Kind, Node, Op, Rec, Src};

use super::state::{records, screen, state, timeline};
use super::*;

/// The hand clock's wall time: every log of these tests reads it.
const WALL: u64 = 1_792_000_000_000;
/// The hand clock's monotonic time.
const MONO: u64 = 5_000;

fn hand_clock(rig: &Rig, sid: &str) {
    timeline(rig, sid, |timeline| {
        timeline
            .slog_mut()
            .set_clock(crate::session_log::Clock::reading(|| {
                aterm_slog::Stamp::new(WALL, MONO)
            }));
    });
}

fn holder() -> String {
    format!("claude-code-alab@{}", running().pid)
}

fn sid_actor(sid: &str) -> Actor {
    crate::session_log::sid_actor(sid).expect("a minted session id")
}

/// An `ask` of `kind` that the harness says it answers `will_in_ms` after it (`None`: it waits
/// for a person).
fn ask(kind: AskKind, will_in_ms: Option<u64>) -> Body {
    Body::Ask {
        kind,
        title: Some("Run cargo test?".to_string()),
        options: vec!["yes".to_string(), "no".to_string()],
        will: will_in_ms.map(|_| "yes".to_string()),
        will_in_ms,
    }
}

fn asked(ask: u64, kind: AskKind, by: By, reply: Reply) -> Body {
    Body::Asked {
        ask,
        kind,
        by,
        reply,
        option: None,
        text: None,
    }
}

fn wall(kind: &str) -> Body {
    Body::Wall {
        kind: kind.to_string(),
        resets_ms: None,
    }
}

fn turn(turn: u64, state: TurnState) -> Body {
    Body::Turn {
        turn,
        state,
        by: (state == TurnState::Started).then_some(By::Person),
    }
}

/// A `tool` record of call `id` that ends at once (`done` with no `started`): one step opened and
/// closed, so none stands open after it.
fn tool_done(id: &str) -> Body {
    Body::Tool {
        turn: Some(1),
        id: id.to_string(),
        name: "Bash".to_string(),
        state: WireStep::Done,
        text: None,
    }
}

/// The live records of `sid`'s log of kind `kind`, oldest first.
fn of(rig: &Rig, sid: &str, kind: Kind) -> Vec<Rec> {
    records(rig, sid)
        .into_iter()
        .filter(|rec| rec.k == kind)
        .collect()
}

/// The agent run open in `sid`'s log.
fn agent_run(rig: &Rig, sid: &str) -> Node {
    of(rig, sid, Kind::Agent)
        .iter()
        .rev()
        .find(|rec| rec.op == Op::Open)
        .map(|rec| rec.node)
        .expect("an agent run")
}

/// The steps of kind `kind` that `src` opened in `sid`'s log.
fn opened_by(rig: &Rig, sid: &str, kind: Kind, src: Src) -> usize {
    of(rig, sid, kind)
        .iter()
        .filter(|rec| rec.op == Op::Open && rec.src == src)
        .count()
}

/// `rec` carries the stream's four stamps and record `hseq`'s number: `holder=<the claim's
/// holder>`, `src=harness`, `conf=claim`, `by=<the tab's session>`, `hseq=`.
fn stamped(rec: &Rec, sid: &str, hseq: u64) {
    let holder = holder();
    assert_eq!(
        (
            rec.src,
            rec.conf,
            rec.by,
            rec.f.get_str(Key::Holder),
            rec.f.get_u(Key::Hseq),
        ),
        (
            Src::Harness,
            Conf::Claim,
            sid_actor(sid),
            Some(holder.as_str()),
            Some(hseq)
        ),
        "{rec:?}"
    );
}

/// The needs standing in `sid`'s log, by their words.
fn needs(rig: &Rig, sid: &str) -> Vec<&'static str> {
    timeline(rig, sid, |timeline| {
        timeline
            .slog()
            .log()
            .reader_state()
            .needs
            .iter()
            .flatten()
            .map(|need| need.word)
            .collect()
    })
}

/// `sid`'s published agent word and detail.
fn reads(rig: &Rig, sid: &str) -> (&'static str, Option<String>) {
    timeline(rig, sid, |timeline| {
        let agent = timeline.agent();
        (agent.word, agent.detail.clone())
    })
}

/// Wait until `sid`'s agent word reads `want` (a hang detector).
fn comes_to(rig: &Rig, sid: &str, want: &str) {
    aterm_spec::wait::until_within(SETTLE, &format!("{sid} reads {want}"), || {
        (reads(rig, sid).0 == want).then_some(())
    });
}

/// Send titles `seq` and `seq + 1` and read both acknowledgements: the lane's look after the
/// first is done before it reads the second.
fn settle(stream: &mut Client, seq: u64) {
    assert_eq!(stream.record(seq, title()), (seq, None));
    assert_eq!(stream.record(seq + 1, title()), (seq + 1, None));
}

/// Set the awake clock to `ms` and send `beat` record `seq` there: the harness lives, and the
/// lane looks after it.
fn beat_at(rig: &Rig, stream: &mut Client, seq: u64, ms: u64) {
    rig.service.clock.store(ms, Ordering::Release);
    assert_eq!(stream.record(seq, Body::Beat), (seq, None));
}

/// How many `restart cause=<cause>` rows `sid`'s timeline holds.
fn restarts(rig: &Rig, sid: &str, cause: &str) -> usize {
    let payload = format!("cause={cause}");
    timeline(rig, sid, |timeline| {
        timeline
            .view_copy(None, 0)
            .rows()
            .iter()
            .filter(|row| row.kind == crate::restart_act::RESTART_EVENT && *row.payload == *payload)
            .count()
    })
}

/// The cause of aterm's next `restart` record on `stream`, its other records passed over.
fn heard_restart(stream: &mut Client) -> String {
    loop {
        if let Body::Restart { cause, .. } = stream.own().body {
            return cause;
        }
    }
}

/// Whether `stream` heard a `restart` record of aterm's.
fn heard_any_restart(stream: &Client) -> bool {
    stream
        .heard
        .iter()
        .any(|record| matches!(record.body, Body::Restart { .. }))
}

/// How many `mark agent word=<word> src=aterm` rows `sid`'s log holds.
fn aterm_marks(rig: &Rig, sid: &str, word: &str) -> usize {
    of(rig, sid, Kind::Agent)
        .iter()
        .filter(|rec| rec.src == Src::Aterm && rec.f.get_word(Key::Word) == Some(word))
        .count()
}

/// HX-15i1 EXIT TEST: AN `ask` IS WRITTEN AS AN APPROVAL OR QUESTION STEP AND RAISES THE NEED. An
/// `ask` of an approval opens an approval step under the open turn, with no `box=` (the record
/// names no tool), so its need is `approval:other`; an `ask` of a question opens a question step,
/// its need `question`. Each open carries the four stamps and the record's `seq` as `hseq=`, and
/// the kind `ask` is applied.
/// NEGATIVE HALF: an `ask` whose step the log refuses (it holds as many nodes open as it may)
/// is acknowledged `dropped=refused:log` and opens nothing; the kind leaves the applied set and
/// its stand-down ends with it, so the stream's ask steps close as aterm's and no need of them
/// stands. (The stream's own bound of 64 open steps is never reached first: the log's
/// `MAX_OPEN` of 64 counts the session, its run and its turn too.)
#[test]
fn an_ask_writes_approval_or_question_and_raises_the_need() {
    let rig = Rig::new(2);
    hand_clock(&rig, ONE);
    let mut stream = rig.claimant(ONE, "state,approvals,questions");
    stream.open(ONE);
    assert_eq!(stream.record(1, state(State::Working)), (1, None));
    assert_eq!(stream.record(2, turn(1, TurnState::Started)), (2, None));
    let turn_node = of(&rig, ONE, Kind::Turn)
        .iter()
        .find(|rec| rec.op == Op::Open)
        .map(|rec| rec.node)
        .expect("the stream's turn");
    assert!(needs(&rig, ONE).is_empty());

    assert_eq!(stream.record(3, ask(AskKind::Approval, None)), (3, None));
    let approvals = of(&rig, ONE, Kind::Approval);
    assert_eq!(approvals.len(), 1, "{approvals:?}");
    stamped(&approvals[0], ONE, 3);
    assert_eq!(
        (
            approvals[0].op,
            approvals[0].par,
            approvals[0].f.get_word(Key::Box)
        ),
        (Op::Open, Some(turn_node), None),
        "an approval step under the open turn, no box named"
    );
    assert_eq!(needs(&rig, ONE), ["approval:other"]);

    assert_eq!(stream.record(4, ask(AskKind::Question, None)), (4, None));
    let questions = of(&rig, ONE, Kind::Question);
    assert_eq!(questions.len(), 1, "{questions:?}");
    stamped(&questions[0], ONE, 4);
    assert_eq!(questions[0].op, Op::Open);
    let mut standing = needs(&rig, ONE);
    standing.sort_unstable();
    assert_eq!(standing, ["approval:other", "question"]);
    assert!(rig.state(ONE).applied.contains("ask"));

    // NEGATIVE HALF: asks until the log refuses one's step.
    let mut seq = 4;
    let refused = loop {
        seq += 1;
        assert!(seq < 100, "the log never refused a step");
        let (acked, dropped) = stream.record(seq, ask(AskKind::Approval, None));
        assert_eq!(acked, seq);
        if let Some(why) = dropped {
            break why;
        }
    };
    assert_eq!(refused, "refused:log");
    let applied_approvals = 1 + usize::try_from(seq - 5).expect("a count");
    assert_eq!(
        opened_by(&rig, ONE, Kind::Approval, Src::Harness),
        applied_approvals,
        "the refused ask opened nothing"
    );
    assert!(!rig.state(ONE).applied.contains("ask"));
    let ended_by_aterm = of(&rig, ONE, Kind::Approval)
        .iter()
        .filter(|rec| rec.op == Op::Close && rec.src == Src::Aterm)
        .count();
    assert_eq!(ended_by_aterm, applied_approvals, "the stand-down ended");
    assert!(needs(&rig, ONE).is_empty(), "{:?}", needs(&rig, ONE));
}

/// HX-15i1 EXIT TEST: `asked` NAMES WHO ANSWERED AND WITH WHAT (D31). Each `asked` is written as
/// `mark act word=approve|deny|answer rule=asked as=<who answered>` on its ask's step, stamped
/// with its own `seq`, and closes that step `out=approved|declined|answered`, which lifts the
/// step's need. A person is `as=person`, a peer tab `as=peer:<tab>`, an `aterm ctl` command
/// `as=aterm ctl` and the harness's own rule `as=harness rule` (the log keeps the words `ctl`
/// and `harness` for its own actors).
/// NEGATIVE HALF: an `asked` naming an ask no step holds open still writes its act, on the agent
/// run, and closes nothing.
#[test]
fn asked_names_who_answered_and_with_what() {
    let rig = Rig::new(2);
    hand_clock(&rig, ONE);
    let mut stream = rig.claimant(ONE, "state,approvals,questions");
    stream.open(ONE);
    assert_eq!(stream.record(1, state(State::Asking)), (1, None));
    let cases = [
        (
            AskKind::Approval,
            By::Person,
            Reply::Approve,
            "approve",
            "person",
            "approved",
        ),
        (
            AskKind::Question,
            By::Peer(TWO.to_string()),
            Reply::Answer,
            "answer",
            "peer:t0000000002",
            "answered",
        ),
        (
            AskKind::Approval,
            By::Ctl,
            Reply::Deny,
            "deny",
            "aterm ctl",
            "declined",
        ),
        (
            AskKind::Question,
            By::Harness,
            Reply::Answer,
            "answer",
            "harness rule",
            "answered",
        ),
    ];
    let mut seq = 1;
    for (kind, by, reply, word, answerer, out) in cases {
        seq += 1;
        let asked_seq = seq + 1;
        assert_eq!(stream.record(seq, ask(kind, None)), (seq, None));
        let step = timeline(&rig, ONE, |timeline| timeline.slog().stream_ask_node(seq))
            .expect("the ask's step");
        assert!(!needs(&rig, ONE).is_empty(), "the ask raised its need");
        assert_eq!(
            stream.record(asked_seq, asked(seq, kind, by, reply)),
            (asked_seq, None)
        );
        let act = of(&rig, ONE, Kind::Act)
            .into_iter()
            .last()
            .expect("the asked act");
        stamped(&act, ONE, asked_seq);
        assert_eq!(
            (
                act.op,
                act.node,
                act.f.get_word(Key::Word),
                act.f.get_str(Key::Rule),
                act.f.get_str(Key::As),
            ),
            (Op::Mark, step, Some(word), Some("asked"), Some(answerer)),
            "{act:?}"
        );
        let close = records(&rig, ONE)
            .into_iter()
            .find(|rec| rec.node == step && rec.op == Op::Close)
            .expect("the ask's step closed");
        stamped(&close, ONE, asked_seq);
        assert_eq!(close.f.get_word(Key::Out), Some(out));
        assert!(needs(&rig, ONE).is_empty(), "the answer lifted the need");
        seq = asked_seq;
    }

    // NEGATIVE HALF: an `asked` of an ask no step holds open.
    let closes = |rig: &Rig| {
        records(rig, ONE)
            .iter()
            .filter(|rec| rec.op == Op::Close)
            .count()
    };
    let before = closes(&rig);
    let run = agent_run(&rig, ONE);
    seq += 1;
    assert_eq!(
        stream.record(
            seq,
            asked(99, AskKind::Approval, By::Person, Reply::Approve)
        ),
        (seq, None)
    );
    let act = of(&rig, ONE, Kind::Act)
        .into_iter()
        .last()
        .expect("the act");
    stamped(&act, ONE, seq);
    assert_eq!(
        (act.node, act.f.get_word(Key::Word), act.f.get_str(Key::As)),
        (run, Some("approve"), Some("person")),
        "written on the agent run"
    );
    assert_eq!(closes(&rig), before, "and it closed nothing");
}

/// HX-15i1 EXIT TEST: A `wall` RECORD WRITES A WALL STEP AND THE WORD `wall:<kind>`. Under a
/// `walled` state, a `wall` of a kind aterm knows opens a wall step `wall=<kind>` under the agent
/// run, stamped, raising `wall:<kind>`, and the agent word becomes `wall:<kind>`, written by the
/// stream's arm. A `wall` of another kind passes the first (`out=passed`) and opens its own; a
/// `state` that is no wall passes the last, and its need ends.
/// NEGATIVE HALF: a `wall` of a kind aterm does not know is a wall step with no `wall=` and the
/// bare word `wall`, never a guessed kind.
#[test]
fn a_wall_record_writes_a_wall_step_and_the_word_wall_kind() {
    let rig = Rig::new(2);
    hand_clock(&rig, ONE);
    let mut stream = rig.claimant(ONE, "state,walls");
    stream.open(ONE);
    assert_eq!(stream.record(1, state(State::Walled)), (1, None));
    assert_eq!(reads(&rig, ONE), ("wall", None), "no kind yet");
    let run = agent_run(&rig, ONE);

    assert_eq!(stream.record(2, wall("usage-session")), (2, None));
    assert_eq!(reads(&rig, ONE), ("wall:usage-session", None));
    let node = timeline(&rig, ONE, |timeline| timeline.slog().stream_wall_node())
        .expect("the stream's wall step");
    let opened = records(&rig, ONE)
        .into_iter()
        .find(|rec| rec.node == node && rec.op == Op::Open)
        .expect("its open");
    stamped(&opened, ONE, 2);
    assert_eq!(
        (opened.par, opened.f.get_word(Key::Wall)),
        (Some(run), Some("usage-session")),
        "under the agent run"
    );
    assert_eq!(needs(&rig, ONE), ["wall:usage-session"]);
    let mark = of(&rig, ONE, Kind::Agent)
        .into_iter()
        .last()
        .expect("the agent mark");
    assert_eq!(
        (mark.src, mark.f.get_word(Key::Word)),
        (Src::Harness, Some("wall:usage-session"))
    );

    // Another kind passes the first.
    assert_eq!(stream.record(3, wall("overloaded")), (3, None));
    assert_eq!(reads(&rig, ONE), ("wall:overloaded", None));
    let passed = records(&rig, ONE)
        .into_iter()
        .find(|rec| rec.node == node && rec.op == Op::Close)
        .expect("the first wall closed");
    stamped(&passed, ONE, 3);
    assert_eq!(passed.f.get_word(Key::Out), Some("passed"));
    assert_eq!(needs(&rig, ONE), ["wall:overloaded"]);

    // A state that is no wall passes the last.
    assert_eq!(stream.record(4, state(State::Working)), (4, None));
    assert_eq!(reads(&rig, ONE), ("busy", None));
    assert!(timeline(&rig, ONE, |timeline| timeline.slog().stream_wall_node()).is_none());
    assert!(needs(&rig, ONE).is_empty());

    // NEGATIVE HALF: a kind aterm does not know is the bare `wall`.
    assert_eq!(stream.record(5, state(State::Walled)), (5, None));
    assert_eq!(stream.record(6, wall("tokens")), (6, None));
    assert_eq!(reads(&rig, ONE), ("wall", None));
    let node = timeline(&rig, ONE, |timeline| timeline.slog().stream_wall_node())
        .expect("the stream's wall step");
    let opened = records(&rig, ONE)
        .into_iter()
        .find(|rec| rec.node == node && rec.op == Op::Open)
        .expect("its open");
    stamped(&opened, ONE, 6);
    assert_eq!(opened.f.get_word(Key::Wall), None, "no kind guessed");
    assert_eq!(needs(&rig, ONE), ["wall"]);
}

/// HX-15i1 EXIT TEST: `asking` AND `walled` TAKE THEIR KIND FROM AN APPLIED `ask` OR `wall`
/// RECORD FIRST (D13), over what the screen shows: `asking` reads `question` after an `ask` of a
/// question and `prompt` after one of an approval, with no detail; `walled` reads the standing
/// `wall`'s kind, and the bare `wall` for a kind aterm does not know.
/// NEGATIVE HALF: with no `ask` applied `asking` reads the screen's kind; once the stream's wall
/// passed, a later `walled` with no new `wall` reads the screen's kind again, not the old one.
#[test]
fn asking_and_walled_take_their_kind_from_an_applied_ask_or_wall_record() {
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "state,approvals,questions,walls");
    stream.open(ONE);

    // NEGATIVE HALF first: no `ask` yet, the screen's kind.
    assert_eq!(stream.record(1, state(State::Asking)), (1, None));
    screen(&rig, ONE, "prompt", Some("bash:not-read-only"));
    assert_eq!(
        reads(&rig, ONE),
        ("prompt", Some("bash:not-read-only".to_string()))
    );

    // The records' kinds, whatever the screen shows.
    assert_eq!(stream.record(2, ask(AskKind::Question, None)), (2, None));
    assert_eq!(reads(&rig, ONE), ("question", None));
    assert_eq!(stream.record(3, ask(AskKind::Approval, None)), (3, None));
    assert_eq!(reads(&rig, ONE), ("prompt", None));
    screen(&rig, ONE, "question", None);
    assert_eq!(reads(&rig, ONE), ("prompt", None), "the screen's question");

    assert_eq!(stream.record(4, state(State::Walled)), (4, None));
    screen(&rig, ONE, "wall:usage-session", None);
    assert_eq!(
        reads(&rig, ONE),
        ("wall:usage-session", None),
        "no wall yet"
    );
    assert_eq!(stream.record(5, wall("model-bucket")), (5, None));
    assert_eq!(reads(&rig, ONE), ("wall:model-bucket", None));
    screen(&rig, ONE, "wall:usage-weekly", None);
    assert_eq!(reads(&rig, ONE), ("wall:model-bucket", None));
    assert_eq!(stream.record(6, wall("tokens")), (6, None));
    assert_eq!(
        reads(&rig, ONE),
        ("wall", None),
        "the record's unknown kind, not the screen's"
    );

    // NEGATIVE HALF: the wall passed; a later `walled` has no record's kind.
    assert_eq!(stream.record(7, state(State::Working)), (7, None));
    assert_eq!(stream.record(8, state(State::Walled)), (8, None));
    assert_eq!(reads(&rig, ONE), ("wall:usage-weekly", None));
}

/// HX-15i1 EXIT TEST: THE PROMPT, QUESTION AND WALL READERS STAND DOWN FOR EACH KIND THE STREAM
/// APPLIED (D29). On a tab whose word the screen decides (the claim holds no `state`), the
/// screen's box and wall each open a step of `src=screen` until the stream applies `ask` or
/// `wall`; that record closes the screen's step `out=passed` and opens its own, and from then a
/// box or wall the screen shows opens nothing. Per parser: a claim holding `approvals` and not
/// `questions` stands only the prompt parser down, so the screen's question dialog still opens a
/// step.
/// NEGATIVE HALF: a renewal that drops `approvals` and `walls` ends both stand-downs: the
/// stream's steps close `out=passed src=aterm`, the wall the screen still shows opens its step
/// at once, and the screen's readers open steps again for what it shows next.
#[test]
fn the_prompt_question_and_wall_readers_stand_down_for_each_kind_the_stream_applied() {
    let rig = Rig::new(2);
    hand_clock(&rig, ONE);
    let mut stream = rig.claimant(ONE, "approvals,walls");
    let id = stream.id();
    stream.open(ONE);
    let screen_steps = |kind| opened_by(&rig, ONE, kind, Src::Screen);

    // The prompt parser's box, before any `ask`.
    screen(&rig, ONE, "prompt", Some("bash"));
    assert_eq!(screen_steps(Kind::Approval), 1);
    let screens = of(&rig, ONE, Kind::Approval);
    assert_eq!(stream.record(1, ask(AskKind::Approval, None)), (1, None));
    let closed = records(&rig, ONE)
        .into_iter()
        .find(|rec| rec.node == screens[0].node && rec.op == Op::Close)
        .expect("the screen's step closed");
    assert_eq!(closed.f.get_word(Key::Out), Some("passed"));
    assert_eq!(opened_by(&rig, ONE, Kind::Approval, Src::Harness), 1);
    assert_eq!(needs(&rig, ONE), ["approval:other"], "one step, one need");
    // From here the screen's boxes open nothing.
    screen(&rig, ONE, "busy", None);
    screen(&rig, ONE, "prompt", Some("edit"));
    assert_eq!(
        screen_steps(Kind::Approval),
        1,
        "the prompt parser stood down"
    );
    // The question parser did not: the claim holds no `questions`. (Its dialog is the box
    // `question`; the bare word `question` is a rest, which opens no step.)
    screen(&rig, ONE, "prompt", Some("question"));
    assert_eq!(screen_steps(Kind::Question), 1, "the question parser feeds");

    // The wall readers.
    screen(&rig, ONE, "wall:usage-session", None);
    assert_eq!(screen_steps(Kind::Wall), 1);
    assert_eq!(stream.record(2, wall("usage-session")), (2, None));
    assert_eq!(opened_by(&rig, ONE, Kind::Wall, Src::Harness), 1);
    screen(&rig, ONE, "busy", None);
    screen(&rig, ONE, "wall:spend", None);
    assert_eq!(screen_steps(Kind::Wall), 1, "the wall readers stood down");
    assert_eq!(
        needs(&rig, ONE)
            .into_iter()
            .filter(|need| need.starts_with("wall"))
            .collect::<Vec<_>>(),
        ["wall:usage-session"],
        "the stream's wall alone"
    );

    // NEGATIVE HALF: a renewal without `approvals` or `walls` ends both stand-downs.
    place(
        &rig.service.handle(ONE),
        4,
        &[Duty::State],
        id,
        running(),
        "claude-code-alab",
    )
    .expect("a narrower grant");
    let passed_by_aterm = |kind| {
        of(&rig, ONE, kind)
            .iter()
            .filter(|rec| {
                rec.op == Op::Close
                    && rec.src == Src::Aterm
                    && rec.f.get_word(Key::Out) == Some("passed")
            })
            .count()
    };
    assert_eq!(passed_by_aterm(Kind::Approval), 1, "the stream's ask step");
    assert_eq!(passed_by_aterm(Kind::Wall), 1, "the stream's wall step");
    assert_eq!(
        screen_steps(Kind::Wall),
        2,
        "the wall still shown (`wall:spend`) opens its step"
    );
    screen(&rig, ONE, "prompt", Some("bash"));
    assert_eq!(
        screen_steps(Kind::Approval),
        2,
        "the prompt parser feeds again"
    );
    screen(&rig, ONE, "wall:usage-session", None);
    assert_eq!(screen_steps(Kind::Wall), 3, "and the wall readers");
}

/// HX-15i1 EXIT TEST: AN `ask` STILL OPEN `restart_act::GRACE` PAST ITS ANSWER TIME RAISES THE
/// NEED AND THE RESTART. The harness said it answers `will_in_ms` after the record: on the awake
/// clock, `GRACE` past that, with its beats still coming, the tab reads `wall:unresponsive`
/// (written by aterm), whose stall step raises `unresponsive`, and the watchdog writes one
/// `restart cause=stall`, which the open stream carries as a record. Its `asked` ends the stall.
/// NEGATIVE HALF: one millisecond short of the grace nothing is read or written; an `ask` that
/// names no answer time (the harness waits for a person) never stalls.
#[test]
fn an_ask_open_past_its_answer_time_raises_the_need_and_the_restart() {
    const WILL: u64 = 10_000;
    let grace = u64::try_from(crate::restart_act::GRACE.as_millis()).expect("ms");
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "state,approvals");
    stream.open(ONE);
    let t0 = rig.service.clock.load(Ordering::Acquire);
    assert_eq!(stream.record(1, state(State::Asking)), (1, None));
    assert_eq!(stream.record(2, Body::Beat), (2, None));
    assert_eq!(
        stream.record(3, ask(AskKind::Approval, Some(WILL))),
        (3, None)
    );
    assert_eq!(reads(&rig, ONE).0, "prompt");

    // NEGATIVE HALF: one millisecond short of the grace.
    beat_at(&rig, &mut stream, 4, t0 + WILL + grace - 1);
    settle(&mut stream, 5);
    assert_eq!(reads(&rig, ONE).0, "prompt");
    assert_eq!(restarts(&rig, ONE, "stall"), 0);
    assert!(!heard_any_restart(&stream));
    assert!(!needs(&rig, ONE).contains(&"unresponsive"));

    beat_at(&rig, &mut stream, 7, t0 + WILL + grace);
    comes_to(&rig, ONE, "wall:unresponsive");
    assert_eq!(heard_restart(&mut stream), "stall");
    assert_eq!(restarts(&rig, ONE, "stall"), 1);
    assert_eq!(
        aterm_marks(&rig, ONE, "wall:unresponsive"),
        1,
        "written by aterm"
    );
    assert!(
        needs(&rig, ONE).contains(&"unresponsive"),
        "{:?}",
        needs(&rig, ONE)
    );
    assert!(
        needs(&rig, ONE).contains(&"approval:other"),
        "the ask's own need stands"
    );

    // Still unanswered: one restart for the one stall.
    beat_at(&rig, &mut stream, 8, t0 + WILL + grace + 20_000);
    settle(&mut stream, 9);
    assert_eq!(restarts(&rig, ONE, "stall"), 1);

    // The answer ends the stall.
    assert_eq!(
        stream.record(11, asked(3, AskKind::Approval, By::Harness, Reply::Approve)),
        (11, None)
    );
    comes_to(&rig, ONE, "prompt");
    assert!(!needs(&rig, ONE).contains(&"unresponsive"));

    // NEGATIVE HALF: an ask with no answer time never stalls.
    let t1 = t0 + WILL + grace + 20_000;
    assert_eq!(stream.record(12, ask(AskKind::Approval, None)), (12, None));
    beat_at(&rig, &mut stream, 13, t1 + 10 * (WILL + grace));
    settle(&mut stream, 14);
    assert_eq!(reads(&rig, ONE).0, "prompt");
    assert_eq!(restarts(&rig, ONE, "stall"), 1);
}

/// HX-15i1 EXIT TEST: NO TURN PROGRESS PAST THE STALL BOUND RAISES THE NEED AND THE RESTART. A
/// turn the stream holds open, its beats still coming, with no record of progress for the
/// policy's `stall_term_after_s` (600 s by default) reads `wall:unresponsive` (written by aterm),
/// whose stall step raises `unresponsive`, and the watchdog writes one `restart cause=stall`,
/// carried as a record. A record of progress ends the stall and starts the count again: the next
/// stall is restarted again.
/// NEGATIVE HALF: one millisecond short of the bound nothing is read or written; a turn that
/// ended is not watched; and an open ask with no answer time waits on a person, no stalled turn.
#[test]
fn no_turn_progress_past_the_stall_bound_raises_the_need_and_the_restart() {
    const BOUND: u64 = 600_000;
    let rig = Rig::new(2);
    rig.service.stall_after_ms.store(BOUND, Ordering::Release);
    let mut stream = rig.claimant(ONE, "state,approvals");
    stream.open(ONE);
    let t0 = rig.service.clock.load(Ordering::Acquire);
    assert_eq!(stream.record(1, state(State::Working)), (1, None));
    assert_eq!(stream.record(2, turn(1, TurnState::Started)), (2, None));

    // NEGATIVE HALF: one millisecond short of the bound.
    beat_at(&rig, &mut stream, 3, t0 + BOUND - 1);
    settle(&mut stream, 4);
    assert_eq!(reads(&rig, ONE).0, "busy");
    assert_eq!(restarts(&rig, ONE, "stall"), 0);
    assert!(!heard_any_restart(&stream));

    beat_at(&rig, &mut stream, 6, t0 + BOUND);
    comes_to(&rig, ONE, "wall:unresponsive");
    assert_eq!(heard_restart(&mut stream), "stall");
    assert_eq!(restarts(&rig, ONE, "stall"), 1);
    assert_eq!(
        aterm_marks(&rig, ONE, "wall:unresponsive"),
        1,
        "written by aterm"
    );
    assert!(
        needs(&rig, ONE).contains(&"unresponsive"),
        "{:?}",
        needs(&rig, ONE)
    );
    beat_at(&rig, &mut stream, 7, t0 + BOUND + 20_000);
    settle(&mut stream, 8);
    assert_eq!(restarts(&rig, ONE, "stall"), 1, "one restart for one stall");

    // Progress ends the stall, and the count starts again from it.
    let t1 = t0 + BOUND + 20_000;
    assert_eq!(stream.record(10, tool_done("toolu_01")), (10, None));
    comes_to(&rig, ONE, "busy");
    assert!(!needs(&rig, ONE).contains(&"unresponsive"));
    beat_at(&rig, &mut stream, 11, t1 + BOUND - 1);
    settle(&mut stream, 12);
    assert_eq!(reads(&rig, ONE).0, "busy");
    beat_at(&rig, &mut stream, 14, t1 + BOUND);
    comes_to(&rig, ONE, "wall:unresponsive");
    assert_eq!(heard_restart(&mut stream), "stall");
    assert_eq!(restarts(&rig, ONE, "stall"), 2);

    // NEGATIVE HALF: an open ask that waits on a person is no stalled turn.
    let t2 = t1 + BOUND;
    assert_eq!(stream.record(15, ask(AskKind::Approval, None)), (15, None));
    comes_to(&rig, ONE, "busy");
    beat_at(&rig, &mut stream, 16, t2 + 3 * BOUND);
    settle(&mut stream, 17);
    assert_eq!(reads(&rig, ONE).0, "busy");
    assert_eq!(restarts(&rig, ONE, "stall"), 2);

    // NEGATIVE HALF: a turn that ended is not watched.
    let ask_step = timeline(&rig, ONE, |timeline| timeline.slog().stream_ask_node(15));
    assert!(ask_step.is_some());
    assert_eq!(
        stream.record(19, asked(15, AskKind::Approval, By::Person, Reply::Approve)),
        (19, None)
    );
    assert_eq!(stream.record(20, turn(1, TurnState::Done)), (20, None));
    beat_at(&rig, &mut stream, 21, t2 + 10 * BOUND);
    settle(&mut stream, 22);
    assert_eq!(reads(&rig, ONE).0, "busy");
    assert_eq!(restarts(&rig, ONE, "stall"), 2);
}

/// The watchdog's `restart_act::GRACE`, in milliseconds.
fn grace_ms() -> u64 {
    u64::try_from(crate::restart_act::GRACE.as_millis()).expect("ms")
}

/// Set what the kernel reports of the claimant's stop state.
fn stopped(rig: &Rig, state: Option<bool>) {
    *rig.service
        .stopped
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = state;
}

/// HX-15i1 REVIEW: A BOX OR WALL STILL SHOWN AS ITS STAND-DOWN ENDS OPENS ITS SCREEN STEP AGAIN.
/// On a tab whose word the screen decides, the screen shows a permission box and the stream's
/// `ask` stands the prompt parser down, its step the box's one step. A renewal that drops
/// `approvals` closes the stream's step `out=passed src=aterm`, and the box the screen still
/// shows, its word unchanged, opens its `src=screen` step again, so a need of it stands. The
/// same for a wall under `walls`.
/// NEGATIVE HALF: a renewal that keeps `approvals` keeps the stand-down and opens nothing.
#[test]
fn a_box_or_wall_still_shown_as_its_stand_down_ends_opens_its_screen_step_again() {
    let rig = Rig::new(2);
    hand_clock(&rig, ONE);
    hand_clock(&rig, TWO);
    let screen_steps = |sid, kind| opened_by(&rig, sid, kind, Src::Screen);
    let renew = |sid: &str, id: u64, duties: &[Duty]| {
        place(
            &rig.service.handle(sid),
            4,
            duties,
            id,
            running(),
            "claude-code-alab",
        )
        .expect("a renewal");
    };

    // The box.
    let mut stream = rig.claimant(ONE, "approvals");
    let id = stream.id();
    stream.open(ONE);
    screen(&rig, ONE, "prompt", Some("bash"));
    assert_eq!(stream.record(1, ask(AskKind::Approval, None)), (1, None));
    assert_eq!(screen_steps(ONE, Kind::Approval), 1);
    assert_eq!(opened_by(&rig, ONE, Kind::Approval, Src::Harness), 1);
    // NEGATIVE HALF: the scope kept.
    renew(ONE, id, &[Duty::Approvals, Duty::State]);
    assert_eq!(
        screen_steps(ONE, Kind::Approval),
        1,
        "the stand-down stands"
    );
    renew(ONE, id, &[Duty::State]);
    assert_eq!(
        reads(&rig, ONE),
        ("prompt", Some("bash".to_string())),
        "the screen's word, unchanged"
    );
    assert_eq!(
        screen_steps(ONE, Kind::Approval),
        2,
        "the box still shown opens its step again"
    );
    let standing = needs(&rig, ONE);
    assert!(
        standing.iter().any(|need| need.starts_with("approval")),
        "{standing:?}"
    );

    // The wall.
    let mut walls = rig.claimant(TWO, "walls");
    let id = walls.id();
    walls.open(TWO);
    screen(&rig, TWO, "wall:usage-session", None);
    assert_eq!(walls.record(1, wall("usage-session")), (1, None));
    assert_eq!(screen_steps(TWO, Kind::Wall), 1);
    renew(TWO, id, &[Duty::State]);
    assert_eq!(reads(&rig, TWO).0, "wall:usage-session");
    assert_eq!(
        screen_steps(TWO, Kind::Wall),
        2,
        "the wall still shown opens its step again"
    );
    assert_eq!(needs(&rig, TWO), ["wall:usage-session"]);
}

/// HX-15i1 REVIEW: AN ASK IS ANSWERED ONCE AND ONLY AS ITS OWN KIND. An `asked` of an applied
/// approval `ask` that names the kind `question` is acknowledged `dropped=refused:ask` and writes
/// nothing: the ask's step stays open with its need. Its own answer closes it; a second `asked`
/// of the same ask is refused `ask` too, so one act stands for the one ask.
/// NEGATIVE HALF: a question's ask answered as a question is applied: the refusal is by the
/// ask's own kind.
#[test]
fn an_ask_is_answered_once_and_only_as_its_own_kind() {
    let rig = Rig::new(2);
    hand_clock(&rig, ONE);
    let mut stream = rig.claimant(ONE, "state,approvals,questions");
    stream.open(ONE);
    let refused = Some("refused:ask".to_string());
    let acts = |rig: &Rig| of(rig, ONE, Kind::Act).len();
    assert_eq!(stream.record(1, state(State::Asking)), (1, None));
    assert_eq!(stream.record(2, ask(AskKind::Approval, None)), (2, None));
    let step =
        timeline(&rig, ONE, |timeline| timeline.slog().stream_ask_node(2)).expect("the ask's step");

    assert_eq!(
        stream.record(3, asked(2, AskKind::Question, By::Person, Reply::Answer)),
        (3, refused.clone())
    );
    assert_eq!(acts(&rig), 0, "nothing written");
    assert_eq!(
        timeline(&rig, ONE, |timeline| timeline.slog().stream_ask_node(2)),
        Some(step),
        "the step stands"
    );
    assert_eq!(needs(&rig, ONE), ["approval:other"]);

    assert_eq!(
        stream.record(4, asked(2, AskKind::Approval, By::Person, Reply::Approve)),
        (4, None)
    );
    assert_eq!(acts(&rig), 1);
    assert!(needs(&rig, ONE).is_empty());
    assert_eq!(
        stream.record(5, asked(2, AskKind::Approval, By::Person, Reply::Deny)),
        (5, refused)
    );
    assert_eq!(acts(&rig), 1, "one act for the one ask");

    // NEGATIVE HALF: a question answered as one.
    assert_eq!(stream.record(6, ask(AskKind::Question, None)), (6, None));
    assert_eq!(
        stream.record(7, asked(6, AskKind::Question, By::Person, Reply::Answer)),
        (7, None)
    );
    assert_eq!(acts(&rig), 2);
}

/// HX-15i1 REVIEW: `asking` READS NO KIND FROM AN ASK WHOSE SCOPE THE CLAIM DROPPED (D13). After
/// an `ask` of a question, a renewal that keeps `approvals` and drops `questions` gives the
/// question parser back; a later `asking` then reads the screen's kind (its `prompt` with the
/// box), not the stale question's.
/// NEGATIVE HALF: while the claim holds `questions`, `asking` reads the record's `question`
/// over the screen's box.
#[test]
fn asking_reads_no_kind_from_an_ask_whose_scope_the_claim_dropped() {
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "state,approvals,questions");
    let id = stream.id();
    stream.open(ONE);
    screen(&rig, ONE, "prompt", Some("bash"));
    assert_eq!(stream.record(1, state(State::Asking)), (1, None));
    assert_eq!(stream.record(2, ask(AskKind::Question, None)), (2, None));
    // NEGATIVE HALF first: the record's kind.
    assert_eq!(reads(&rig, ONE), ("question", None));
    assert_eq!(
        stream.record(3, asked(2, AskKind::Question, By::Person, Reply::Answer)),
        (3, None)
    );

    place(
        &rig.service.handle(ONE),
        4,
        &[Duty::State, Duty::Approvals],
        id,
        running(),
        "claude-code-alab",
    )
    .expect("a narrower grant");
    assert_eq!(stream.record(4, state(State::Working)), (4, None));
    assert_eq!(stream.record(5, state(State::Asking)), (5, None));
    assert_eq!(
        reads(&rig, ONE),
        ("prompt", Some("bash".to_string())),
        "the screen's kind"
    );
}

/// HX-15i1 REVIEW: A STANDING WALL NAMES THE KIND OF A WALL THE SCREEN READS. On a tab whose word
/// the screen decides (the claim holds `walls` and no `state`), the stream's `wall` stands its
/// readers down and raises `wall:<kind>`: where the screen reads a wall of another kind, the
/// word reads the record's kind, so the word and the need name one wall.
/// NEGATIVE HALF: where the screen reads no wall (`busy`), the word is the screen's.
#[test]
fn a_standing_wall_names_the_kind_of_a_wall_the_screen_reads() {
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "approvals,walls");
    stream.open(ONE);
    screen(&rig, ONE, "wall:usage-session", None);
    assert_eq!(stream.record(1, wall("usage-session")), (1, None));
    assert_eq!(needs(&rig, ONE), ["wall:usage-session"]);
    screen(&rig, ONE, "wall:usage-weekly", None);
    assert_eq!(reads(&rig, ONE), ("wall:usage-session", None));
    assert_eq!(needs(&rig, ONE), ["wall:usage-session"]);

    // NEGATIVE HALF: no wall on the screen.
    screen(&rig, ONE, "busy", None);
    assert_eq!(reads(&rig, ONE), ("busy", None));
}

/// HX-15i1 REVIEW: `stall_term = false` KEEPS A STALLED AGENT A WARNING (principle 3). A turn past
/// the stall bound reads `wall:unresponsive`, written by aterm, and raises the need
/// `unresponsive`, but no `restart cause=stall` is written or sent, so no watch would ever
/// signal the harness.
/// NEGATIVE HALF: under the policy's default (`stall_term` on) the same stall is restarted.
#[test]
fn stall_term_false_keeps_a_stalled_agent_a_warning() {
    const BOUND: u64 = 600_000;
    let rig = Rig::new(2);
    rig.service.stall_after_ms.store(BOUND, Ordering::Release);
    rig.service
        .stall_term
        .store(false, std::sync::atomic::Ordering::Release);
    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    let t0 = rig.service.clock.load(Ordering::Acquire);
    assert_eq!(stream.record(1, state(State::Working)), (1, None));
    assert_eq!(stream.record(2, turn(1, TurnState::Started)), (2, None));
    beat_at(&rig, &mut stream, 3, t0 + BOUND);
    comes_to(&rig, ONE, "wall:unresponsive");
    settle(&mut stream, 4);
    assert!(
        needs(&rig, ONE).contains(&"unresponsive"),
        "{:?}",
        needs(&rig, ONE)
    );
    assert_eq!(restarts(&rig, ONE, "stall"), 0);
    assert!(!heard_any_restart(&stream));

    // NEGATIVE HALF: the default restarts it.
    rig.service
        .stall_term
        .store(true, std::sync::atomic::Ordering::Release);
    let mut other = rig.claimant(TWO, "state");
    other.open(TWO);
    let t1 = rig.service.clock.load(Ordering::Acquire);
    assert_eq!(other.record(1, state(State::Working)), (1, None));
    assert_eq!(other.record(2, turn(1, TurnState::Started)), (2, None));
    beat_at(&rig, &mut other, 3, t1 + BOUND);
    comes_to(&rig, TWO, "wall:unresponsive");
    assert_eq!(heard_restart(&mut other), "stall");
    assert_eq!(restarts(&rig, TWO, "stall"), 1);
}

/// HX-15i1 REVIEW: A HAND ON THE TAB HOLDS THE PROGRESS WATCH. A harness holds its answer while a
/// person holds the tab (principle 5): an ask past its answer time under the hold reads no stall
/// and writes no restart, and the count starts again as the hold lifts, so the harness has its
/// whole `will_in_ms` again from there.
/// NEGATIVE HALF: `GRACE` past that answer time with no hand on the tab, it is a stall and is
/// restarted.
#[test]
fn a_hand_on_the_tab_holds_the_progress_watch() {
    const WILL: u64 = 10_000;
    let grace = grace_ms();
    let rig = Rig::new(2);
    let ctx = Arc::clone(&rig.service.handle(ONE).ctx);
    let hold = |on: bool| {
        crate::fabric::apply_hold_for_test(
            ctx.as_ref(),
            on.then(|| crate::fabric::Hold {
                reason: "test".to_string(),
                origin: "local".to_string(),
            }),
        );
    };
    let mut stream = rig.claimant(ONE, "state,approvals");
    stream.open(ONE);
    let t0 = rig.service.clock.load(Ordering::Acquire);
    assert_eq!(stream.record(1, state(State::Asking)), (1, None));
    assert_eq!(stream.record(2, Body::Beat), (2, None));
    assert_eq!(
        stream.record(3, ask(AskKind::Approval, Some(WILL))),
        (3, None)
    );
    hold(true);
    beat_at(&rig, &mut stream, 4, t0 + WILL + grace);
    settle(&mut stream, 5);
    assert_eq!(
        reads(&rig, ONE).0,
        "prompt",
        "no stall while the hold stands"
    );
    let lifted = t0 + 3 * (WILL + grace);
    beat_at(&rig, &mut stream, 7, lifted);
    settle(&mut stream, 8);
    assert_eq!(reads(&rig, ONE).0, "prompt");
    assert_eq!(restarts(&rig, ONE, "stall"), 0);

    // The hold lifts at `lifted`: the ask's time runs again from there.
    hold(false);
    settle(&mut stream, 10);
    beat_at(&rig, &mut stream, 12, lifted + WILL + grace - 1);
    settle(&mut stream, 13);
    assert_eq!(reads(&rig, ONE).0, "prompt");
    assert_eq!(restarts(&rig, ONE, "stall"), 0);
    assert!(!heard_any_restart(&stream));

    // NEGATIVE HALF: past it, with no hand.
    beat_at(&rig, &mut stream, 15, lifted + WILL + grace);
    comes_to(&rig, ONE, "wall:unresponsive");
    assert_eq!(heard_restart(&mut stream), "stall");
}

/// HX-15i1 REVIEW: A STOPPED CLAIMANT IS NOT READ STALLED, AND ITS RESUME STARTS THE PROGRESS
/// COUNT AGAIN. A claimant the kernel stops while its beats still count reads `stopped`, not
/// `wall:unresponsive`, when its ask's answer time runs out, and no restart is written to it;
/// once it runs again, its ask has its whole `will_in_ms` from the resume.
/// NEGATIVE HALF: `GRACE` past that, still unanswered, it is a stall and is restarted.
#[test]
fn a_stopped_claimant_is_not_read_stalled_and_its_resume_starts_the_progress_count_again() {
    const WILL: u64 = 10_000;
    let grace = grace_ms();
    let rig = Rig::new(2);
    let mut stream = rig.claimant(ONE, "state,approvals");
    stream.open(ONE);
    let t0 = rig.service.clock.load(Ordering::Acquire);
    assert_eq!(stream.record(1, state(State::Asking)), (1, None));
    assert_eq!(stream.record(2, Body::Beat), (2, None));
    assert_eq!(
        stream.record(3, ask(AskKind::Approval, Some(WILL))),
        (3, None)
    );
    stopped(&rig, Some(true));
    // Its beats still count (the last came at `t0`) as its ask's time runs out.
    let resumed = t0 + WILL + grace;
    rig.service.clock.store(resumed, Ordering::Release);
    settle(&mut stream, 4);
    comes_to(&rig, ONE, "stopped");
    assert_eq!(restarts(&rig, ONE, "stall"), 0);
    assert!(!heard_any_restart(&stream));

    // It runs again at `resumed`.
    stopped(&rig, Some(false));
    comes_to(&rig, ONE, "prompt");
    beat_at(&rig, &mut stream, 6, resumed + WILL + grace - 1);
    settle(&mut stream, 7);
    assert_eq!(reads(&rig, ONE).0, "prompt");
    assert_eq!(restarts(&rig, ONE, "stall"), 0);

    // NEGATIVE HALF: past it, running.
    beat_at(&rig, &mut stream, 9, resumed + WILL + grace);
    comes_to(&rig, ONE, "wall:unresponsive");
    assert_eq!(heard_restart(&mut stream), "stall");
}

/// HX-15i1 REVIEW: A RECONNECT INSIDE ONE STALL RESTARTS IT NO MORE. An ask past its answer time
/// is restarted once; the stream closes and the same claimant opens a new one (a re-exec in
/// place, or a plain reconnect) with the ask still unanswered. The new stream's count starts at
/// its open, so it is not read stalled at once; once the ask's time runs out again from there it
/// reads `wall:unresponsive`, and no second restart is written: the claim acted on that stall.
/// NEGATIVE HALF: the answer and a new ask make a new stall, which is restarted.
#[test]
fn a_reconnect_inside_one_stall_restarts_it_no_more() {
    const WILL: u64 = 10_000;
    let grace = grace_ms();
    let rig = Rig::new(2);
    let mut first = rig.claimant(ONE, "state,approvals");
    let id = first.id();
    first.open(ONE);
    let t0 = rig.service.clock.load(Ordering::Acquire);
    assert_eq!(first.record(1, state(State::Asking)), (1, None));
    assert_eq!(first.record(2, Body::Beat), (2, None));
    assert_eq!(
        first.record(3, ask(AskKind::Approval, Some(WILL))),
        (3, None)
    );
    beat_at(&rig, &mut first, 4, t0 + WILL + grace);
    comes_to(&rig, ONE, "wall:unresponsive");
    assert_eq!(heard_restart(&mut first), "stall");
    assert_eq!(restarts(&rig, ONE, "stall"), 1);
    drop(first);
    rig.ended(id);

    // The same claimant opens a new stream at `reopened`.
    let reopened = t0 + WILL + grace;
    let mut again = rig.claimant(ONE, "state,approvals");
    again.open(ONE);
    settle(&mut again, 5);
    comes_to(&rig, ONE, "prompt");
    beat_at(&rig, &mut again, 7, reopened + WILL + grace - 1);
    settle(&mut again, 8);
    assert_eq!(reads(&rig, ONE).0, "prompt", "counted from the open");
    beat_at(&rig, &mut again, 10, reopened + WILL + grace);
    comes_to(&rig, ONE, "wall:unresponsive");
    settle(&mut again, 11);
    assert_eq!(restarts(&rig, ONE, "stall"), 1, "the claim acted on it");
    assert!(!heard_any_restart(&again));

    // NEGATIVE HALF: the answer, and a new ask that stalls.
    assert_eq!(
        again.record(13, asked(3, AskKind::Approval, By::Harness, Reply::Approve)),
        (13, None)
    );
    comes_to(&rig, ONE, "prompt");
    let t2 = reopened + WILL + grace;
    assert_eq!(
        again.record(14, ask(AskKind::Approval, Some(WILL))),
        (14, None)
    );
    beat_at(&rig, &mut again, 15, t2 + WILL + grace);
    comes_to(&rig, ONE, "wall:unresponsive");
    assert_eq!(heard_restart(&mut again), "stall");
    assert_eq!(restarts(&rig, ONE, "stall"), 2);
}

/// HX-15i1 REVIEW: A WALL INSIDE ONE STALL IS NO NEW STALL. A turn past the stall bound is
/// restarted once; a `wall` and the `state` that passes it pause the turn rule and move no
/// progress, so the same stall reads again after them and writes no second restart.
/// NEGATIVE HALF: a record of progress makes a new stall, which is restarted.
#[test]
fn a_wall_inside_one_stall_is_no_new_stall() {
    const BOUND: u64 = 600_000;
    let rig = Rig::new(2);
    rig.service.stall_after_ms.store(BOUND, Ordering::Release);
    let mut stream = rig.claimant(ONE, "state,walls");
    stream.open(ONE);
    let t0 = rig.service.clock.load(Ordering::Acquire);
    assert_eq!(stream.record(1, state(State::Working)), (1, None));
    assert_eq!(stream.record(2, turn(1, TurnState::Started)), (2, None));
    beat_at(&rig, &mut stream, 3, t0 + BOUND);
    comes_to(&rig, ONE, "wall:unresponsive");
    assert_eq!(heard_restart(&mut stream), "stall");
    assert_eq!(restarts(&rig, ONE, "stall"), 1);

    assert_eq!(stream.record(4, state(State::Walled)), (4, None));
    assert_eq!(stream.record(5, wall("usage-session")), (5, None));
    comes_to(&rig, ONE, "wall:usage-session");
    assert_eq!(stream.record(6, state(State::Working)), (6, None));
    comes_to(&rig, ONE, "wall:unresponsive");
    settle(&mut stream, 7);
    assert_eq!(restarts(&rig, ONE, "stall"), 1, "the same stall");

    // NEGATIVE HALF: progress, then a stall again.
    let t1 = t0 + BOUND;
    assert_eq!(stream.record(9, tool_done("toolu_01")), (9, None));
    comes_to(&rig, ONE, "busy");
    beat_at(&rig, &mut stream, 10, t1 + BOUND);
    comes_to(&rig, ONE, "wall:unresponsive");
    assert_eq!(heard_restart(&mut stream), "stall");
    assert_eq!(restarts(&rig, ONE, "stall"), 2);
}

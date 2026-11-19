// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The conversation records (`turn`, `tool`, `subagent`, `msg`, `diff`), the tab's Out ring and
//! `want` (HX-15h), driven through the same rig as the stream's own tests: a real claim on a real
//! connection, the shipping lanes and reader, and records written by the wire library's codec.
//! The built-in sources are called as their own callers call them: the screen reader's publish
//! arm (`publish_agent`, through `screen`), the host's settle wake (`SessionLog::tick`) and the
//! transcript feeder (`apply_batch_timed`).

use alab_wire::record::{By, Step as WireStep, TurnState};
use aterm_agent::harness::steps::{
    Step, StepBatch, StepEvent, StepId, StepPos, StepState, ToolName,
};
use aterm_slog::{Actor, Conf, Key, Kind, Lvl, Op, Rec, Src};

use super::state::{records, screen, state, timeline};
use super::*;
use crate::session_timeline::StreamKinds;

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

fn turn(turn: u64, state: TurnState, by: Option<By>) -> Body {
    Body::Turn { turn, state, by }
}

fn tool(id: &str, name: &str, state: WireStep, text: Option<String>) -> Body {
    Body::Tool {
        turn: Some(1),
        id: id.to_string(),
        name: name.to_string(),
        state,
        text,
    }
}

fn subagent(id: &str, state: WireStep) -> Body {
    Body::Subagent {
        id: id.to_string(),
        title: None,
        state,
    }
}

/// The line the test client writes for record `seq` (`ms=0`), as the wire codec writes it.
fn line(seq: u64, body: Body) -> String {
    Record { seq, ms: 0, body }.line().expect("a record line")
}

/// The live records of `sid`'s log of kind `kind`, oldest first.
fn of(rig: &Rig, sid: &str, kind: Kind) -> Vec<Rec> {
    records(rig, sid)
        .into_iter()
        .filter(|rec| rec.k == kind)
        .collect()
}

/// The turn rows of `sid`'s log with `op`.
fn turns(rig: &Rig, sid: &str, op: Op) -> Vec<Rec> {
    records(rig, sid)
        .into_iter()
        .filter(|rec| rec.node.lvl == Lvl::Turn && rec.op == op)
        .collect()
}

/// What `sid`'s current-state record keeps of the stream's other kinds.
fn kinds(rig: &Rig, sid: &str) -> StreamKinds {
    timeline(rig, sid, |timeline| timeline.agent().streamed.clone())
}

/// The kinds `sid`'s stream applied.
fn applied(rig: &Rig, sid: &str) -> Vec<&'static str> {
    rig.state(sid).applied.iter().copied().collect()
}

/// The Out ring's entries of `sid`: `(kind, hseq, fp, body)`, oldest first.
fn ring(rig: &Rig, sid: &str) -> Vec<(&'static str, u64, String, String)> {
    timeline(rig, sid, |timeline| {
        timeline
            .out_ring()
            .entries()
            .map(|e| (e.kind, e.hseq, e.fp.clone(), e.body.clone()))
            .collect()
    })
}

/// Release the claim `id` of `sid`, as `meta set supervisor -` does.
fn release(rig: &Rig, sid: &str, id: u64) {
    assert!(crate::session_timeline::release_supervisor(
        &rig.service.handle(sid).ctx,
        Some(id),
        crate::metrics::now_us(),
        Actor::Ctl,
    ));
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

/// The transcript feeder hands `sid`'s log one batch of `steps` (each at its offset), the
/// file's binding first where `bind`.
fn transcript(rig: &Rig, sid: &str, bind: bool, steps: Vec<(u64, StepEvent)>) {
    let step = |off: u64, ev: StepEvent| Step {
        pos: StepPos {
            fp: 1,
            id: None,
            off,
            at_ms: WALL,
        },
        state: StepState::Partial,
        ev,
    };
    let batch = StepBatch {
        steps: bind
            .then(|| step(0, StepEvent::Bind { len: 0 }))
            .into_iter()
            .chain(steps.into_iter().map(|(off, ev)| step(off, ev)))
            .collect(),
        ..StepBatch::default()
    };
    let handle = rig.service.handle(sid);
    let mut clock = 0;
    crate::session_log::transcript::apply_batch_timed(
        &handle.ctx.timeline,
        crate::session_log::transcript::Source::Transcript,
        &batch,
        (false, None),
        &mut || {
            clock += 1;
            clock
        },
        &mut |_, _| {},
    );
}

fn step_id(id: &str) -> StepId {
    StepId::new(id).expect("a step id")
}

fn tool_use(id: &str, subagent: bool) -> StepEvent {
    StepEvent::ToolUse {
        id: step_id(id),
        tool: ToolName::new(if subagent { "Task" } else { "Bash" }),
        cls: "exec",
        subagent,
    }
}

/// The steps of kind `kind` in `sid`'s log that `src` opened, by their `vref=`.
fn opened_by(rig: &Rig, sid: &str, kind: Kind, src: Src) -> Vec<String> {
    of(rig, sid, kind)
        .iter()
        .filter(|rec| rec.op == Op::Open && rec.src == src)
        .filter_map(|rec| rec.f.get_str(Key::Vref).map(str::to_string))
        .collect()
}

/// A RECORDED STREAM WRITES `turn`, `tool` AND `subagent` ROWS AS HARNESS CLAIMS: the turn
/// (`turn_by=person`), a tool step under it (`tool=Bash vref=<id>`), its close `out=ok`, a
/// subagent step and its close `out=interrupted`, and the turn's close `out=done`, every row
/// stamped `holder= hseq= src=harness conf=claim by=<the tab>`, and each kind applied.
/// NEGATIVE HALF: a claim without `state` has every one dropped for its scope, writing no row
/// and applying nothing.
#[test]
fn a_recorded_stream_writes_turn_tool_and_subagent_rows_as_harness_claims() {
    let rig = Rig::new(2);
    hand_clock(&rig, ONE);
    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    let bodies = [
        turn(1, TurnState::Started, Some(By::Person)),
        tool("toolu_01", "Bash", WireStep::Started, None),
        tool("toolu_01", "Bash", WireStep::Done, None),
        subagent("agent_01", WireStep::Started),
        subagent("agent_01", WireStep::Interrupted),
        turn(1, TurnState::Done, None),
    ];
    for (seq, body) in (1..).zip(bodies.clone()) {
        assert_eq!(stream.record(seq, body), (seq, None), "record {seq}");
    }
    assert_eq!(applied(&rig, ONE), ["subagent", "tool", "turn"]);

    let opened = turns(&rig, ONE, Op::Open);
    assert_eq!(opened.len(), 1, "{opened:?}");
    stamped(&opened[0], ONE, 1);
    assert_eq!(opened[0].f.get_word(Key::TurnBy), Some("person"));
    let t = opened[0].node;

    let tools = of(&rig, ONE, Kind::Tool);
    assert_eq!(
        tools.iter().map(|rec| rec.op).collect::<Vec<_>>(),
        [Op::Open, Op::Close]
    );
    stamped(&tools[0], ONE, 2);
    stamped(&tools[1], ONE, 3);
    assert_eq!(
        (
            tools[0].par,
            tools[0].f.get_str(Key::Tool),
            tools[0].f.get_str(Key::Vref)
        ),
        (Some(t), Some("Bash"), Some("toolu_01")),
        "under the turn"
    );
    assert_eq!(tools[1].f.get_word(Key::Out), Some("ok"));
    assert!(tools[0].f.get(Key::Fp).is_none(), "nothing spilled");
    assert_eq!(
        tools[0].f.get_word(Key::Cls),
        Some("shell"),
        "classed as the transcript classes a `Bash` call"
    );

    let subagents = of(&rig, ONE, Kind::Subagent);
    assert_eq!(
        subagents.iter().map(|rec| rec.op).collect::<Vec<_>>(),
        [Op::Open, Op::Close]
    );
    stamped(&subagents[0], ONE, 4);
    stamped(&subagents[1], ONE, 5);
    assert_eq!(subagents[0].f.get_str(Key::Vref), Some("agent_01"));
    assert_eq!(subagents[0].f.get_word(Key::Cls), Some("agent"));
    assert_eq!(subagents[1].f.get_word(Key::Out), Some("interrupted"));

    let closed = turns(&rig, ONE, Op::Close);
    assert_eq!(closed.len(), 1);
    stamped(&closed[0], ONE, 6);
    assert_eq!(closed[0].f.get_word(Key::Out), Some("done"));

    // NEGATIVE HALF: no `state` in the grant.
    let mut walls = rig.claimant(TWO, "walls");
    walls.open(TWO);
    for (seq, body) in (1..).zip(bodies) {
        assert_eq!(
            walls.record(seq, body),
            (seq, Some("scope".to_string())),
            "record {seq}"
        );
    }
    assert!(applied(&rig, TWO).is_empty());
    let harness_rows = records(&rig, TWO)
        .into_iter()
        .filter(|rec| rec.src == Src::Harness)
        .count();
    assert_eq!(harness_rows, 0, "CONTROL: no row");
}

/// THE TRANSCRIPT FEEDER STANDS DOWN FOR EACH KIND THE STREAM APPLIED (D29), in one tab: once
/// the stream applied `tool`, the transcript's tool call opens no step while its subagent call
/// still does; once it applied `subagent` too, neither opens one; a result closes the step the
/// transcript opened before its kind stood down. NEGATIVE HALF: before any of them is applied
/// the transcript opens both, and when the claim ends it opens both again.
#[test]
fn the_transcript_feeder_stands_down_for_each_kind_the_stream_applied() {
    let rig = Rig::new(2);
    hand_clock(&rig, ONE);
    // The screen opens the run and its turn; the transcript's prompt names it.
    screen(&rig, ONE, "busy", None);
    transcript(
        &rig,
        ONE,
        true,
        vec![
            (
                10,
                StepEvent::Prompt {
                    vref: step_id("prompt_01"),
                },
            ),
            (20, tool_use("toolu_a", false)),
            (30, tool_use("agent_a", true)),
        ],
    );
    assert_eq!(
        (
            opened_by(&rig, ONE, Kind::Tool, Src::Transcript),
            opened_by(&rig, ONE, Kind::Subagent, Src::Transcript)
        ),
        (vec!["toolu_a".to_string()], vec!["agent_a".to_string()]),
        "NEGATIVE HALF: before the stream applies either kind the transcript feeds both"
    );

    let mut stream = rig.claimant(ONE, "state");
    let id = stream.id();
    stream.open(ONE);
    assert_eq!(
        stream.record(1, tool("toolu_s", "Bash", WireStep::Started, None)),
        (1, None)
    );
    transcript(
        &rig,
        ONE,
        false,
        vec![
            (40, tool_use("toolu_b", false)),
            (50, tool_use("agent_b", true)),
            (
                60,
                StepEvent::ToolResult {
                    id: step_id("toolu_a"),
                    error: false,
                },
            ),
        ],
    );
    assert_eq!(
        opened_by(&rig, ONE, Kind::Tool, Src::Transcript),
        ["toolu_a"],
        "`tool` stood down"
    );
    assert_eq!(
        opened_by(&rig, ONE, Kind::Subagent, Src::Transcript),
        ["agent_a", "agent_b"],
        "`subagent` is still the transcript's"
    );
    let closed_a = of(&rig, ONE, Kind::Tool)
        .into_iter()
        .any(|rec| rec.op == Op::Close && rec.src == Src::Transcript);
    assert!(closed_a, "the result closes the step the transcript opened");
    assert_eq!(opened_by(&rig, ONE, Kind::Tool, Src::Harness), ["toolu_s"]);

    assert_eq!(
        stream.record(2, subagent("agent_s", WireStep::Started)),
        (2, None)
    );
    transcript(
        &rig,
        ONE,
        false,
        vec![
            (70, tool_use("toolu_c", false)),
            (80, tool_use("agent_c", true)),
        ],
    );
    assert_eq!(
        (
            opened_by(&rig, ONE, Kind::Tool, Src::Transcript),
            opened_by(&rig, ONE, Kind::Subagent, Src::Transcript)
        ),
        (
            vec!["toolu_a".to_string()],
            vec!["agent_a".to_string(), "agent_b".to_string()]
        ),
        "both stood down"
    );

    // NEGATIVE HALF: the claim ends, and the transcript feeds both again.
    release(&rig, ONE, id);
    transcript(
        &rig,
        ONE,
        false,
        vec![
            (90, tool_use("toolu_d", false)),
            (100, tool_use("agent_d", true)),
        ],
    );
    assert_eq!(
        (
            opened_by(&rig, ONE, Kind::Tool, Src::Transcript),
            opened_by(&rig, ONE, Kind::Subagent, Src::Transcript)
        ),
        (
            vec!["toolu_a".to_string(), "toolu_d".to_string()],
            vec![
                "agent_a".to_string(),
                "agent_b".to_string(),
                "agent_d".to_string()
            ]
        ),
        "the claim ended: the transcript feeds both"
    );
}

/// A STREAM TURN WINS OVER A SCREEN TURN ALREADY OPEN: the screen's `busy` opened the turn
/// (`src=screen`); the stream's `started` takes it (an amend stamped `turn_by= hseq= holder=
/// src=harness conf=claim by=<the tab>`) and opens no second turn; the screen's rest, held far
/// past the settle, closes nothing, and the stream's `done` closes it. NEGATIVE CONTROL: in the
/// other tab, with no stream, the same screen closes its turn at the settle.
#[test]
fn a_stream_turn_wins_over_a_screen_turn_already_open() {
    let rig = Rig::new(2);
    let settle = |rig: &Rig, sid: &str| {
        let later = aterm_slog::Stamp::new(
            WALL + 20 * aterm_slog::SETTLE_MS,
            MONO + 20 * aterm_slog::SETTLE_MS,
        );
        timeline(rig, sid, |timeline| {
            let _ = timeline.slog_mut().tick(later);
        });
    };
    for sid in [ONE, TWO] {
        hand_clock(&rig, sid);
        screen(&rig, sid, "busy", None);
    }
    let screen_turn = turns(&rig, ONE, Op::Open);
    assert_eq!(screen_turn.len(), 1);
    assert_eq!(screen_turn[0].src, Src::Screen);

    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    assert_eq!(
        stream.record(1, turn(1, TurnState::Started, Some(By::Ctl))),
        (1, None)
    );
    assert_eq!(turns(&rig, ONE, Op::Open).len(), 1, "no second turn");
    let taken: Vec<Rec> = records(&rig, ONE)
        .into_iter()
        .filter(|rec| rec.op == Op::Amend && rec.node == screen_turn[0].node)
        .collect();
    assert_eq!(taken.len(), 1, "{taken:?}");
    stamped(&taken[0], ONE, 1);
    assert_eq!(taken[0].f.get_word(Key::TurnBy), Some("ctl"));

    for sid in [ONE, TWO] {
        screen(&rig, sid, "idle", None);
        settle(&rig, sid);
    }
    assert!(
        turns(&rig, ONE, Op::Close).is_empty(),
        "the screen's rest closes nothing"
    );
    assert_eq!(
        turns(&rig, TWO, Op::Close).len(),
        1,
        "CONTROL: no stream, the screen's rest closes its turn"
    );
    screen(&rig, ONE, "busy", None);
    assert_eq!(turns(&rig, ONE, Op::Open).len(), 1, "the screen opens none");

    assert_eq!(stream.record(2, turn(1, TurnState::Done, None)), (2, None));
    let closed = turns(&rig, ONE, Op::Close);
    assert_eq!(closed.len(), 1);
    stamped(&closed[0], ONE, 2);
    assert_eq!(closed[0].f.get_word(Key::Out), Some("done"));
}

/// A RECORD OVER ONE LOG RECORD WRITES `hseq=` AND `fp=` AND ITS BODY TO THE OUT RING: a tool
/// call naming its text (content no row carries; its line over 512 bytes) opens its step with
/// `hseq=` and `fp=<the first 40 hex of the SHA-256 of its line>`, and the ring keeps its line,
/// escaped, under the same `hseq` and `fp`; a tool name over `tool=`'s 64 bytes spills the same
/// way, though its line is short. NEGATIVE CONTROL: the call's short `done` writes `hseq=` and no
/// `fp=`, and the ring keeps nothing of it.
#[test]
fn a_record_over_one_log_record_writes_hseq_and_fp_and_its_body_to_the_out_ring() {
    let rig = Rig::new(2);
    hand_clock(&rig, ONE);
    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    let long = tool(
        "toolu_01",
        "Bash",
        WireStep::Started,
        Some(format!("cargo test {}", "x ".repeat(400))),
    );
    let long_line = line(1, long.clone());
    assert!(long_line.len() > aterm_slog::MAX_RECORD_BYTES);
    assert_eq!(stream.record(1, long), (1, None));
    let fp = crate::out_ring::fp(&long_line);
    assert_eq!(fp, {
        let mut hex =
            aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(long_line.as_bytes()));
        hex.truncate(40);
        hex
    });
    let opened = of(&rig, ONE, Kind::Tool);
    assert_eq!(opened.len(), 1);
    stamped(&opened[0], ONE, 1);
    assert_eq!(opened[0].f.get_str(Key::Fp), Some(fp.as_str()));
    assert!(
        opened[0].f.get(Key::Text).is_none(),
        "the text is the ring's, never the row's"
    );
    let kept = ring(&rig, ONE);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        (kept[0].0, kept[0].1, kept[0].2.as_str()),
        ("tool", 1, fp.as_str())
    );
    assert_eq!(kept[0].3, crate::out_ring::escape(&long_line));
    assert_eq!(
        aterm_codec::pct::decode(&kept[0].3),
        long_line,
        "the whole line"
    );

    // CONTROL: the call's `done` is one log record.
    let done = tool("toolu_01", "Bash", WireStep::Done, None);
    assert!(line(2, done.clone()).len() <= aterm_slog::MAX_RECORD_BYTES);
    assert_eq!(stream.record(2, done), (2, None));
    let closed = of(&rig, ONE, Kind::Tool);
    assert_eq!(closed.len(), 2);
    stamped(&closed[1], ONE, 2);
    assert!(closed[1].f.get(Key::Fp).is_none(), "CONTROL: no fp=");
    assert_eq!(
        ring(&rig, ONE).len(),
        1,
        "CONTROL: the ring keeps nothing of it"
    );

    // A name its row cannot carry whole spills too.
    let name = "n".repeat(70);
    let named = tool("toolu_02", &name, WireStep::Started, None);
    let named_line = line(3, named.clone());
    assert!(named_line.len() <= aterm_slog::MAX_RECORD_BYTES);
    assert_eq!(stream.record(3, named), (3, None));
    let rows = of(&rig, ONE, Kind::Tool);
    assert_eq!(
        rows[2].f.get_str(Key::Fp),
        Some(crate::out_ring::fp(&named_line).as_str())
    );
    assert_eq!(ring(&rig, ONE).len(), 2);
}

/// `msg` AND `diff` ROWS ARE WRITTEN UNDER THEIR OWN KINDS: each is a mark on the agent run,
/// `mark msg|diff hseq= fp= holder=` stamped as a harness claim, its body in the Out ring under
/// its kind's word and never in the row. NEGATIVE CONTROL: neither is written as another
/// kind's row (no `agent`, `tool` or `turn` row stands for them).
#[test]
fn msg_and_diff_rows_are_written_under_their_own_kinds() {
    let rig = Rig::new(2);
    hand_clock(&rig, ONE);
    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    let msg = Body::Msg {
        turn: Some(1),
        by: By::Agent,
        text: "The stream split is done.".to_string(),
    };
    let diff = Body::Diff {
        turn: Some(1),
        path: Some("crates/aterm-gui/src/out_ring.rs".to_string()),
        patch: "@@ -1 +1 @@\n-old\n+new\n".to_string(),
    };
    let lines = [line(1, msg.clone()), line(2, diff.clone())];
    assert_eq!(stream.record(1, msg), (1, None));
    assert_eq!(stream.record(2, diff), (2, None));
    assert_eq!(applied(&rig, ONE), ["diff", "msg"]);

    let run = records(&rig, ONE)
        .into_iter()
        .find(|rec| rec.node.lvl == Lvl::Run && rec.op == Op::Open && rec.k == Kind::Agent)
        .map(|rec| rec.node);
    for (kind, hseq, line) in [(Kind::Msg, 1, &lines[0]), (Kind::Diff, 2, &lines[1])] {
        let rows = of(&rig, ONE, kind);
        assert_eq!(rows.len(), 1, "{kind:?}: {rows:?}");
        assert_eq!(rows[0].op, Op::Mark);
        stamped(&rows[0], ONE, hseq);
        assert_eq!(
            rows[0].f.get_str(Key::Fp),
            Some(crate::out_ring::fp(line).as_str())
        );
        assert!(rows[0].f.get(Key::Text).is_none());
        assert_eq!(Some(rows[0].node), run, "on the agent run");
    }
    let kept = ring(&rig, ONE);
    assert_eq!(
        kept.iter().map(|e| (e.0, e.1)).collect::<Vec<_>>(),
        [("msg", 1), ("diff", 2)]
    );
    assert_eq!(aterm_codec::pct::decode(&kept[1].3), lines[1]);

    // CONTROL: no row of another kind stands for them.
    let other = records(&rig, ONE)
        .into_iter()
        .filter(|rec| {
            rec.src == Src::Harness
                && matches!(rec.f.get_u(Key::Hseq), Some(1 | 2))
                && !matches!(rec.k, Kind::Msg | Kind::Diff)
        })
        .count();
    assert_eq!(other, 0);
}

/// `want` IS SENT WHEN THE OUT RING LACKS THE CONVERSATION: a stream that opens on a ring holding
/// none of its claimant's conversation hears `hand`, `want`, `policy`; once the ring holds a
/// record of that claimant's, the same claimant's stream reopened hears `hand` and `policy`
/// only. NEGATIVE CONTROL: the other tab's ring holds nothing, and its stream hears `want`.
#[test]
fn want_is_sent_when_the_out_ring_lacks_the_conversation() {
    // A lane for the reopened stream, which stays open, and one for each of the other tab's.
    let rig = Rig::new(3);
    let mut first = rig.claimant(ONE, "state");
    let id = first.id();
    let opened = first.open(ONE);
    assert!(first.wanted(), "an empty ring");
    assert_eq!(
        first
            .opening
            .iter()
            .map(|record| (record.seq, record.body.kind().map(|k| k.word())))
            .collect::<Vec<_>>(),
        [
            (opened.sent + 1, Some("hand")),
            (opened.sent + 2, Some("want")),
            (opened.sent + 3, Some("policy")),
        ]
    );
    let msg = Body::Msg {
        turn: None,
        by: By::Person,
        text: "go on".to_string(),
    };
    assert_eq!(first.record(1, msg), (1, None));
    drop(first);
    rig.ended(id);
    rig.lanes_free();

    let mut again = rig.claimant(ONE, "state");
    let reopened = again.open(ONE);
    assert!(
        !again.wanted(),
        "the ring holds the claimant's conversation"
    );
    assert_eq!(
        again
            .opening
            .iter()
            .map(|record| (record.seq, record.body.kind().map(|k| k.word())))
            .collect::<Vec<_>>(),
        [
            (reopened.sent + 1, Some("hand")),
            (reopened.sent + 2, Some("policy")),
        ]
    );

    // A claim not granted the conversation's scope hears no `want`: whatever it sent again
    // would be dropped for its scope.
    let mut walls = rig.claimant(TWO, "walls");
    let walls_id = walls.id();
    walls.open(TWO);
    assert!(!walls.wanted(), "a grant without `state`");
    drop(walls);
    rig.ended(walls_id);

    let mut other = rig.claimant(TWO, "state");
    other.open(TWO);
    assert!(
        other.wanted(),
        "CONTROL: the other tab's ring holds nothing"
    );
}

/// THE OPEN-SUBAGENT COUNT IS KEPT IN THE CURRENT-STATE RECORD: each `subagent started` adds one
/// (a repeated one adds nothing and writes no second step), each `done` or `interrupted` takes
/// one away; `state ended` leaves none open, and the claim's end drops the count with the kind.
/// NEGATIVE CONTROL: a `subagent` dropped for its scope counts nothing.
#[test]
fn the_open_subagent_count_is_kept_in_the_current_state_record() {
    let rig = Rig::new(2);
    hand_clock(&rig, ONE);
    let mut stream = rig.claimant(ONE, "state");
    let id = stream.id();
    stream.open(ONE);
    let mut seq = 0;
    let mut record = |stream: &mut Client, body: Body| {
        seq += 1;
        assert_eq!(stream.record(seq, body), (seq, None), "record {seq}");
    };
    let open = |rig: &Rig| timeline(rig, ONE, |timeline| timeline.open_subagents());
    assert_eq!(open(&rig), 0);
    record(&mut stream, subagent("agent_01", WireStep::Started));
    assert_eq!(open(&rig), 1);
    record(&mut stream, subagent("agent_01", WireStep::Started));
    assert_eq!(open(&rig), 1, "the same subagent again");
    assert_eq!(
        opened_by(&rig, ONE, Kind::Subagent, Src::Harness).len(),
        1,
        "and no second step"
    );
    record(&mut stream, subagent("agent_02", WireStep::Started));
    assert_eq!(open(&rig), 2);
    record(&mut stream, subagent("agent_01", WireStep::Done));
    assert_eq!(open(&rig), 1);
    record(&mut stream, subagent("agent_02", WireStep::Interrupted));
    assert_eq!(open(&rig), 0);

    record(&mut stream, subagent("agent_03", WireStep::Started));
    record(&mut stream, subagent("agent_04", WireStep::Failed));
    assert_eq!(open(&rig), 1, "an end with none open takes nothing away");
    record(&mut stream, state(State::Ended));
    assert_eq!(open(&rig), 0, "the agent ended");

    record(&mut stream, state(State::Working));
    record(&mut stream, subagent("agent_05", WireStep::Started));
    assert_eq!(open(&rig), 1);
    release(&rig, ONE, id);
    assert_eq!(kinds(&rig, ONE).subagent, None, "the claim ended");
    assert_eq!(open(&rig), 0);

    // NEGATIVE CONTROL: dropped for its scope.
    let mut walls = rig.claimant(TWO, "walls");
    walls.open(TWO);
    assert_eq!(
        walls.record(1, subagent("agent_01", WireStep::Started)),
        (1, Some("scope".to_string()))
    );
    assert_eq!(timeline(&rig, TWO, |timeline| timeline.open_subagents()), 0);
}

/// A SUBAGENT WHOSE TURN CLOSED IS NO LONGER OPEN (HX-15h review): the stream's `subagent` step
/// under a turn that ends without the subagent's own end closes with it (`out=gone`), and the
/// current-state record's count drops it, its step being the one truth. Sixty-five such turns
/// later a new subagent still opens: the bound on the stream's open steps counts only steps the
/// log holds open. NEGATIVE CONTROL: a subagent whose turn still runs counts.
#[test]
fn a_subagent_its_turn_ended_leaves_the_open_count() {
    let rig = Rig::new(1);
    hand_clock(&rig, ONE);
    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    let mut seq = 0;
    let mut record = |stream: &mut Client, body: Body| {
        seq += 1;
        assert_eq!(stream.record(seq, body), (seq, None), "record {seq}");
    };
    let open = |rig: &Rig| timeline(rig, ONE, |timeline| timeline.open_subagents());
    for n in 1..=65 {
        record(&mut stream, turn(n, TurnState::Started, Some(By::Person)));
        record(
            &mut stream,
            subagent(&format!("agent_{n:02}"), WireStep::Started),
        );
        assert_eq!(open(&rig), 1, "CONTROL: its turn runs ({n})");
        record(&mut stream, turn(n, TurnState::Interrupted, None));
        assert_eq!(open(&rig), 0, "the turn's close ended it ({n})");
    }
    let gone = of(&rig, ONE, Kind::Subagent)
        .iter()
        .filter(|rec| rec.op == Op::Close && rec.f.get_word(Key::Out) == Some("gone"))
        .count();
    assert_eq!(gone, 65, "each closed with its turn");
    record(&mut stream, turn(66, TurnState::Started, Some(By::Person)));
    record(&mut stream, subagent("agent_66", WireStep::Started));
    assert_eq!(open(&rig), 1, "a new subagent opens");
}

/// A RECORD THAT WRITES NO ROW PUTS NOTHING IN THE OUT RING (HX-15h review): a `tool started`
/// whose text spills it goes to the ring behind its row's `fp=`; the same record sent again,
/// its step still open, writes no row and adds no ring entry, so every entry is one a row
/// names. NEGATIVE CONTROL: the call's end with text, which closes the step, is kept.
#[test]
fn a_record_that_writes_no_row_puts_nothing_in_the_out_ring() {
    let rig = Rig::new(1);
    hand_clock(&rig, ONE);
    let mut stream = rig.claimant(ONE, "state");
    stream.open(ONE);
    let started = tool(
        "toolu_01",
        "Bash",
        WireStep::Started,
        Some("cargo test".to_string()),
    );
    assert_eq!(stream.record(1, started.clone()), (1, None));
    assert_eq!(stream.record(2, started), (2, None));
    let rows = of(&rig, ONE, Kind::Tool);
    assert_eq!(rows.len(), 1, "one step: {rows:?}");
    let kept = ring(&rig, ONE);
    assert_eq!(
        kept.iter().map(|e| (e.0, e.1)).collect::<Vec<_>>(),
        [("tool", 1)],
        "the repeat wrote no row and keeps no entry"
    );
    let named = |hseq: u64| {
        let fp = ring(&rig, ONE)
            .into_iter()
            .find(|e| e.1 == hseq)
            .map(|e| e.2)
            .expect("an entry");
        records(&rig, ONE)
            .iter()
            .any(|rec| rec.f.get_str(Key::Fp) == Some(fp.as_str()))
    };
    assert!(named(1), "its row names it");

    let done = tool("toolu_01", "Bash", WireStep::Done, Some("ok".to_string()));
    assert_eq!(stream.record(3, done), (3, None));
    assert_eq!(
        ring(&rig, ONE).iter().map(|e| e.1).collect::<Vec<_>>(),
        [1, 3],
        "CONTROL: the close is kept"
    );
    assert!(named(3));
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Transcript join replay tests across [`TxEvent`], [`Log`], and [`TurnTracker`].

use aterm_slog::{
    ATTRIBUTION_MS, Actor, BoxWord, CARRY_MAX_LINE, Conf, Key, Kind, Log, Lvl, Mark, Node, Op, Rec,
    ShortId, Src, TX_TOOLS_MAX, TurnTracker, TxEvent, TxPos, TxState, Val, Verdict, WallJudge,
    WallKind,
};

use crate::conformance_transcript::project_parts;
use crate::seq_twins::{self, Took};
use crate::support::{WALL0, agent_session, at, lines, time_order_holds};

const PROCESS_START: u64 = WALL0 - 60_000;
const FP: u64 = 0xc1a0_de00_0000_0001;
const ID: Option<(u64, u64)> = Some((16_777_220, 4_242));

struct Rig {
    log: Log,
    run: Node,
    tr: TurnTracker,

    /// Test ms.
    now: u64,
    off: u64,
    state: TxState,

    /// Its birth order and the host calls it made, while the twins record
    /// (`seq_twins`, plan 5B-3).
    born: usize,
    path: Vec<Took>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        seq_twins::died(self.born, std::mem::take(&mut self.path), &self.log);
    }
}

impl Rig {
    fn new() -> Rig {
        let mut log = Log::new(1);
        let run = agent_session(&mut log, 0);
        Rig {
            log,
            run,
            tr: TurnTracker::new(run),
            now: 0,
            off: 0,
            state: TxState::On,
            born: seq_twins::born(),
            path: Vec::new(),
        }
    }

    fn took(&mut self, step: String, seq0: u64) {
        if seq_twins::on() {
            let state = (self.state != TxState::Off).then(|| {
                project_parts(
                    &self.log,
                    self.run,
                    &self.tr,
                    self.now,
                    self.state == TxState::Partial,
                    0,
                )
            });
            let lines = seq_twins::lines_since(&self.log, seq0);
            self.path.push(Took { step, state, lines });
        }
    }

    fn bound() -> Rig {
        Rig::bound_at(TxState::On)
    }

    fn bound_at(state: TxState) -> Rig {
        let mut r = Rig::new();
        r.state = state;
        r.bind(0);
        r
    }

    fn wall(&self) -> u64 {
        WALL0 + self.now
    }

    const fn pos(&self, off: u64, wall_ms: u64) -> TxPos {
        TxPos {
            fp: FP,
            id: ID,
            off,
            wall_ms,
        }
    }

    fn since(&self, seq0: u64) -> Vec<Rec> {
        self.log
            .window()
            .filter(|r| r.seq > seq0)
            .cloned()
            .collect()
    }

    fn bind(&mut self, len: u64) -> Vec<Rec> {
        self.off = self.off.max(len);
        let pos = self.pos(len, self.wall());
        self.call(
            TxEvent::Bind {
                len,
                floor_ms: PROCESS_START,
            },
            pos,
        )
    }

    fn call(&mut self, ev: TxEvent<'_>, pos: TxPos) -> Vec<Rec> {
        let seq0 = self.log.last_seq();
        let (state, stamp) = (self.state, at(self.now));
        self.tr.transcript(&mut self.log, ev, pos, state, stamp);
        self.took(seq_twins::tx_step(&ev, &pos, state, self.now), seq0);
        self.since(seq0)
    }

    fn tx(&mut self, ev: TxEvent<'_>) -> Vec<Rec> {
        let wall = self.wall();
        self.tx_at(ev, wall)
    }

    fn tx_at(&mut self, ev: TxEvent<'_>, wall_ms: u64) -> Vec<Rec> {
        self.off += 200;
        let pos = self.pos(self.off, wall_ms);
        self.call(ev, pos)
    }

    fn see(&mut self, v: Verdict) -> Vec<Rec> {
        let seq0 = self.log.last_seq();
        let stamp = at(self.now);
        let detail = match v {
            Verdict::Prompt(b) => Some(b.word()),
            _ => None,
        };
        self.tr.observe(&mut self.log, v, detail, stamp);
        self.took(seq_twins::see_step(v, detail, self.now), seq0);
        self.since(seq0)
    }

    fn adopt(&mut self, line: &str) {
        let seq0 = self.log.last_seq();
        self.tr = TurnTracker::from_carry_line(line, at(self.now)).expect("the carry line reads");
        self.took(seq_twins::adopt_step(line, self.now), seq0);
    }

    fn wait(&mut self, ms: u64) {
        let (seq0, from) = (self.log.last_seq(), self.now);
        self.now += ms;
        if self.tr.wake_at().is_some_and(|d| d <= self.now) {
            let stamp = at(self.now);
            self.tr.tick(&mut self.log, stamp);
        }
        self.took(seq_twins::wait_step(ms, from), seq0);
    }

    fn records(&self) -> Vec<Rec> {
        self.log.window().cloned().collect()
    }

    fn turn(&self) -> Node {
        self.tr.turn().expect("a turn is open")
    }

    fn ordered(&self) {
        if let Err(e) = time_order_holds(&lines(&self.log)) {
            panic!("{e}:\n{}", lines(&self.log).join("\n"));
        }
    }
}

fn v(s: &str) -> ShortId {
    ShortId::new(s).expect("an id")
}

fn prompt(s: &str) -> TxEvent<'static> {
    TxEvent::Prompt { vref: v(s) }
}

fn tool(id: &str) -> TxEvent<'static> {
    TxEvent::ToolUse {
        id: v(id),
        tool: "Bash",
        cls: "shell",
        subagent: false,
    }
}

fn result(id: &str, error: bool) -> TxEvent<'static> {
    TxEvent::ToolResult { id: v(id), error }
}

const END: TxEvent<'static> = TxEvent::TurnEnd {
    vdur_ms: 4200,
    tok_in: 900,
    tok_out: 120,
};

fn bash() -> Verdict {
    Verdict::Prompt(BoxWord::from_detail("bash:not-read-only"))
}

fn ops(recs: &[Rec], op: Op, lvl: Lvl) -> Vec<Rec> {
    recs.iter()
        .filter(|r| r.op == op && r.node.lvl == lvl)
        .cloned()
        .collect()
}

fn vref_of(r: &Rec) -> Option<&str> {
    match r.f.get(Key::Vref) {
        Some(Val::Id(id)) => Some(id.as_str()),
        _ => None,
    }
}

fn vendor(r: &Rec, conf: Conf) {
    assert_eq!(
        (r.src, r.by, r.conf),
        (Src::Transcript, Actor::Vendor, conf),
        "{r:?}"
    );
}

#[test]
pub(crate) fn a_prompt_confirms_the_screens_turn_in_its_window() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    let t1 = r.turn();
    r.now = 5_000;
    let recs = r.tx(prompt("v1"));
    assert_eq!(recs.len(), 1, "{recs:?}");
    assert_eq!(
        (recs[0].op, recs[0].node, vref_of(&recs[0])),
        (Op::Amend, t1, Some("v1"))
    );
    vendor(&recs[0], Conf::Exact);
    assert_eq!(r.tr.tx_view().mapped, Some(t1));
    assert!(
        r.log
            .state()
            .turn()
            .is_some_and(|o| o.f.get(Key::Vref).is_some())
    );
    r.ordered();
}

#[test]
pub(crate) fn a_prompt_6s_after_the_turn_opened_does_not_attach() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    let t1 = r.turn();
    r.now = 6_000;
    let recs = r.tx(prompt("v1"));
    assert!(!recs.iter().any(|x| x.op == Op::Amend), "{recs:?}");
    let closed = ops(&recs, Op::Close, Lvl::Turn);
    assert_eq!(
        (closed.len(), closed[0].node, closed[0].out()),
        (1, t1, Some("superseded"))
    );
    assert_eq!(
        closed[0].t,
        WALL0 + 6_000,
        "dated at the prompt: no end was read"
    );
    vendor(&closed[0], Conf::Exact);
    let opened = ops(&recs, Op::Open, Lvl::Turn);
    assert_eq!(opened.len(), 1);
    assert_eq!(
        (opened[0].src, vref_of(&opened[0])),
        (Src::Transcript, Some("v1"))
    );
    assert_eq!(r.tr.turn(), Some(opened[0].node));
    r.ordered();
}

#[test]
pub(crate) fn a_prompt_into_a_turn_holding_another_vendor_turn_supersedes_it() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    let t1 = r.turn();
    r.now = 100;
    r.tx(prompt("v1"));
    r.now = 200;
    r.see(bash());
    r.now = 300;
    let held = r.tx(END);
    assert!(
        ops(&held, Op::Close, Lvl::Turn).is_empty(),
        "the box holds the turn"
    );
    r.now = 400;
    r.see(Verdict::Busy);
    r.now = 900;
    let recs = r.tx(prompt("v2"));
    let closed = ops(&recs, Op::Close, Lvl::Turn);
    assert_eq!((closed[0].node, closed[0].out()), (t1, Some("superseded")));
    assert_eq!(
        closed[0].t,
        WALL0 + 400,
        "V1's end, clamped to the box's close"
    );
    let opened = ops(&recs, Op::Open, Lvl::Turn);
    assert_eq!(vref_of(&opened[0]), Some("v2"));
    r.ordered();
}

#[test]
pub(crate) fn an_approval_open_when_v2s_prompt_comes_keeps_its_turn() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    let t1 = r.turn();
    r.now = 100;
    r.tx(prompt("v1"));
    r.now = 200;
    r.see(bash());
    r.now = 300;
    r.tx(END);
    r.now = 1_000;
    let recs = r.tx(prompt("v2"));
    assert!(
        recs.iter().all(|x| x.op != Op::Close && x.op != Op::Open),
        "{recs:?}"
    );
    assert_eq!(r.tr.tx_view().mapped, Some(t1));
    r.now = 1_100;
    let opened = r.tx(tool("c1"));
    assert_eq!(opened[0].par, Some(t1));
    r.now = 1_200;
    let passed = r.see(Verdict::Busy);
    assert_eq!(ops(&passed, Op::Close, Lvl::Step)[0].out(), Some("passed"));
    assert_eq!(ops(&passed, Op::Close, Lvl::Step)[0].src, Src::Screen);
    r.now = 1_300;
    r.see(Verdict::Idle);
    r.wait(1_000);
    let turns = ops(&r.records(), Op::Close, Lvl::Turn);
    assert_eq!(
        (turns.len(), turns[0].node, turns[0].src),
        (1, t1, Src::Screen)
    );
    assert_eq!(ops(&r.records(), Op::Open, Lvl::Turn).len(), 1, "one turn");
    let recs = r.tx(result("c1", false));
    assert_eq!((recs[0].op, recs[0].out()), (Op::Amend, Some("ok")));
    vendor(&recs[0], Conf::Exact);
    r.ordered();
}

#[test]
pub(crate) fn a_prompt_after_the_settle_confirms_the_closed_turn_late() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    let t1 = r.turn();
    r.now = 100;
    r.see(Verdict::Idle);
    r.wait(1_000);
    assert!(r.tr.turn().is_none());
    let recs = r.tx(prompt("v1"));
    assert_eq!((recs.len(), recs[0].op, recs[0].node), (1, Op::Amend, t1));
    assert_eq!(r.log.state().last[1].as_ref().map(|c| c.node), Some(t1));
    assert_eq!(r.tr.tx_view().mapped, Some(t1));

    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.now = 100;
    r.see(Verdict::Idle);
    r.wait(1_000);
    r.now = 6_100;
    let recs = r.tx(prompt("v1"));
    assert!(!recs.iter().any(|x| x.op == Op::Amend), "{recs:?}");
    let opened = ops(&recs, Op::Open, Lvl::Turn);
    assert_eq!((opened.len(), opened[0].src), (1, Src::Transcript));
    assert_eq!(
        r.tr.wake_at(),
        Some(7_100),
        "the resting word's settle, from now"
    );
    r.wait(1_000);
    let closed = ops(&r.records(), Op::Close, Lvl::Turn);
    assert_eq!(
        (closed.len(), closed[1].src, closed[1].out()),
        (2, Src::Screen, Some("done"))
    );
    r.ordered();
}

#[test]
pub(crate) fn a_prompt_older_than_the_open_turn_does_not_name_it() {
    for state in [TxState::On, TxState::Partial] {
        let mut r = Rig::bound_at(state);
        r.now = 600_000;
        r.see(Verdict::Busy);
        let t1 = r.turn();
        let recs = r.tx_at(prompt("vold"), WALL0 + 1_000);
        assert!(
            !recs.iter().any(|x| vref_of(x).is_some()),
            "{state:?}: a prompt 599 s older than the turn named it: {recs:?}"
        );
        assert_eq!(r.tr.tx_view().mapped, None, "{state:?}");
        assert!(
            !r.tx(tool("c1")).iter().any(|x| x.op == Op::Open),
            "{state:?}"
        );
        assert_eq!(r.tr.tx_view().dropped, 1, "{state:?}");
        let wall = r.wall();
        let recs = r.tx_at(prompt("vnew"), wall - 1_000);
        assert!(
            recs.iter()
                .any(|x| x.op == Op::Amend && x.node == t1 && vref_of(x) == Some("vnew")),
            "{state:?}: {recs:?}"
        );
        r.ordered();
    }
    let mut r = Rig::bound_at(TxState::Partial);
    r.see(Verdict::Busy);
    let t1 = r.turn();
    r.now = 500;
    r.see(Verdict::Idle);
    r.wait(1_000);
    r.now = 3_000;
    r.see(Verdict::Busy);
    let t2 = r.turn();
    let recs = r.tx_at(prompt("v1"), WALL0 + 100);
    let named: Vec<Node> = recs
        .iter()
        .filter(|x| vref_of(x) == Some("v1"))
        .map(|x| x.node)
        .collect();
    assert_eq!(named, vec![t1], "not {t2}: {recs:?}");
    r.ordered();
}

#[test]
pub(crate) fn a_prompt_read_before_the_screens_turn_names_it() {
    for (gap, names) in [(2_000, true), (ATTRIBUTION_MS + 1_000, false)] {
        let mut r = Rig::bound_at(TxState::Partial);
        r.now = 10_000;
        r.tx(prompt("vlead"));
        assert_eq!(r.tr.tx_view().mapped, None);
        r.now += gap;
        let recs = r.see(Verdict::Busy);
        let t = r.turn();
        let named = recs
            .iter()
            .any(|x| x.op == Op::Amend && x.node == t && vref_of(x) == Some("vlead"));
        assert_eq!(named, names, "{gap} ms: {recs:?}");
        let opened = r
            .tx(tool("c1"))
            .iter()
            .any(|x| x.op == Op::Open && x.par == Some(t));
        assert_eq!(opened, names, "{gap} ms");
        assert_eq!(r.tr.tx_view().dropped, u64::from(!names));
        r.ordered();
    }
}

#[test]
pub(crate) fn a_prompt_the_screen_missed_opens_the_vendor_turn() {
    let mut r = Rig::bound();
    r.now = 400;
    r.tr.note_input(Actor::Human, at(400));
    r.now = 500;
    let recs = r.tx(prompt("v1"));
    let opened = ops(&recs, Op::Open, Lvl::Turn);
    assert_eq!(opened.len(), 1);
    let t = opened[0].clone();
    assert_eq!(
        (t.src, t.conf, t.by, vref_of(&t)),
        (Src::Transcript, Conf::Exact, Actor::Human, Some("v1"))
    );
    assert_eq!(r.tr.wake_at(), None);
    r.see(Verdict::Busy);
    assert_eq!(
        ops(&r.records(), Op::Open, Lvl::Turn).len(),
        1,
        "busy opens no second"
    );
    r.now = 2_000;
    let recs = r.tx(END);
    let closed = ops(&recs, Op::Close, Lvl::Turn);
    assert_eq!((closed[0].node, closed[0].out()), (t.node, Some("done")));
    r.ordered();
}

#[test]
pub(crate) fn tool_calls_open_under_the_mapped_turn_and_results_close_them() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    let t1 = r.turn();
    r.tx(prompt("v1"));
    r.now = 100;
    let recs = r.tx(tool("c1"));
    let p = recs[0].clone();
    assert_eq!((p.op, p.k, p.par), (Op::Open, Kind::Tool, Some(t1)));
    assert_eq!(p.f.get_word(Key::Cls), Some("shell"));
    assert_eq!(vref_of(&p), Some("c1"));
    assert!(matches!(p.f.get(Key::Tool), Some(Val::Text(t)) if t.as_str() == "Bash"));
    vendor(&p, Conf::Exact);
    let sub = r.tx(TxEvent::ToolUse {
        id: v("c2"),
        tool: "Task",
        cls: "agent",
        subagent: true,
    });
    assert_eq!(sub[0].k, Kind::Subagent);
    assert!(r.tx(tool("c1")).is_empty(), "a repeated call id");
    r.now = 800;
    let closed = r.tx(result("c1", true));
    assert_eq!(
        (closed[0].op, closed[0].node, closed[0].out()),
        (Op::Close, p.node, Some("error"))
    );
    assert_eq!(closed[0].f.get_u(Key::DurMs), Some(700));
    assert_eq!(closed[0].f.get_word(Key::Dur), Some("wall"));
    assert_eq!(r.tx(result("c2", false))[0].out(), Some("ok"));
    r.ordered();
}

#[test]
pub(crate) fn a_tool_use_for_a_closed_or_unmapped_turn_never_opens_a_step() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.tx(prompt("v1"));
    r.see(Verdict::Idle);
    r.wait(1_000);
    assert!(r.tr.turn().is_none());
    assert!(r.tx(tool("c1")).is_empty());
    assert_eq!(r.tr.tx_view().dropped, 1);
    let mut r = Rig::new();
    r.state = TxState::Partial;
    r.bind(0);
    r.tx(prompt("v1"));
    assert_eq!(
        (r.tr.tx_view().vref, r.tr.tx_view().mapped),
        (Some(v("v1")), None)
    );
    assert!(r.tx(tool("c1")).is_empty());
    assert_eq!(r.tr.tx_view().dropped, 1);
    assert!(!r.records().iter().any(|x| x.k == Kind::Tool));
}

#[test]
pub(crate) fn a_turn_end_closes_the_turn_and_then_amends_its_numbers() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    let t1 = r.turn();
    r.tx(prompt("v1"));
    for (i, step) in [
        bash(),
        Verdict::Prompt(BoxWord::QUESTION),
        Verdict::Wall(WallKind::Overloaded),
        Verdict::Unresponsive,
    ]
    .into_iter()
    .enumerate()
    {
        r.now += 10;
        r.see(step);
        r.now += 10;
        r.see(Verdict::Busy);
        r.tx(tool(&format!("c{i}")));
        r.tx(result(&format!("c{i}"), i == 1));
    }
    r.log
        .mark(
            Mark::new(Kind::Mail, Src::Aterm, Conf::Exact).at(t1),
            at(r.now),
        )
        .expect("a mail mark");
    r.now += 100;
    let recs = r.tx(END);
    let close = ops(&recs, Op::Close, Lvl::Turn)[0].clone();
    vendor(&close, Conf::Exact);
    assert_eq!((close.node, close.out()), (t1, Some("done")));
    for k in [
        Key::Steps,
        Key::Appr,
        Key::Qs,
        Key::Walls,
        Key::Stalls,
        Key::Mail,
        Key::Errs,
    ] {
        assert!(close.f.get(k).is_some(), "{k:?} on {close:?}");
    }
    assert!(close.f.get(Key::Tools).is_some(), "tools= kept: {close:?}");
    assert!(
        close.f.get(Key::VdurMs).is_none(),
        "the numbers are not on the close"
    );
    let n = close.f.iter().count();
    assert!(
        n + 3 > aterm_slog::MAX_FIELDS,
        "{n} fields: the numbers would cut tools="
    );
    let amend = recs
        .iter()
        .find(|x| x.op == Op::Amend)
        .expect("the numbers' amend");
    assert!(amend.seq > close.seq);
    assert_eq!(
        (
            amend.f.get_u(Key::VdurMs),
            amend.f.get_u(Key::TokIn),
            amend.f.get_u(Key::TokOut)
        ),
        (Some(4200), Some(900), Some(120))
    );
    let lc = r.tr.view(r.now).last_close.expect("closed");
    assert!(lc.tx && !lc.ctl);
    r.ordered();
}

#[test]
pub(crate) fn a_turn_end_with_a_screen_step_open_only_amends() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    let t1 = r.turn();
    r.tx(prompt("v1"));
    r.see(bash());
    let recs = r.tx(END);
    assert!(ops(&recs, Op::Close, Lvl::Turn).is_empty(), "{recs:?}");
    assert_eq!((recs[0].op, recs[0].node), (Op::Amend, t1));
    assert!(r.tr.tx_view().ended);
    r.see(Verdict::Busy);
    r.see(Verdict::Idle);
    r.wait(1_000);
    let closed = ops(&r.records(), Op::Close, Lvl::Turn);
    assert_eq!((closed.len(), closed[0].src), (1, Src::Screen));

    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    let t1 = r.turn();
    r.tx(prompt("v1"));
    r.see(Verdict::Idle);
    r.wait(1_000);
    let recs = r.tx(END);
    assert_eq!((recs.len(), recs[0].op, recs[0].node), (1, Op::Amend, t1));
    r.ordered();
}

#[test]
pub(crate) fn a_turn_end_takes_the_settles_outcome_and_the_interrupt() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.tx(prompt("v1"));
    r.see(Verdict::Question);
    let c = ops(&r.tx(END), Op::Close, Lvl::Turn)[0].clone();
    assert_eq!(
        (c.out(), c.conf, c.src),
        (Some("asked"), Conf::Heuristic, Src::Transcript)
    );
    assert_eq!(r.tr.wake_at(), None, "the settle went with the turn");

    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.tx(prompt("v1"));
    assert!(r.tx(TxEvent::Interrupt).is_empty());
    assert!(r.tr.tx_view().ended, "Esc ended the vendor turn");
    r.see(Verdict::Idle);
    r.wait(1_000);
    let c = ops(&r.records(), Op::Close, Lvl::Turn)[0].clone();
    assert_eq!((c.out(), c.src), (Some("interrupted"), Src::Screen));
    assert!(r.tx(END).is_empty(), "nothing is left to end");
    r.ordered();
}

#[test]
pub(crate) fn an_interrupted_vendor_turn_is_continued_by_no_later_turn() {
    for state in [TxState::On, TxState::Partial] {
        let mut r = Rig::bound_at(state);
        r.see(Verdict::Busy);
        r.tx(prompt("v1"));
        r.tx(tool("c1"));
        r.tx(TxEvent::Interrupt);
        r.tx(result("c1", true));
        r.see(Verdict::Idle);
        r.wait(1_000);
        assert_eq!(r.tr.turn(), None, "{state:?}");
        let closed = ops(&r.records(), Op::Close, Lvl::Turn);
        assert_eq!(closed.len(), 1, "{state:?}: {closed:?}");
        let expected = if state == TxState::On {
            "interrupted"
        } else {
            "done"
        };
        assert_eq!(
            (closed[0].out(), closed[0].src),
            (Some(expected), Src::Screen),
            "only a measured transcript may change the screen's close outcome: {state:?}"
        );
        r.wait(30_000);
        let recs = r.see(Verdict::Busy);
        let t2 = r.turn();
        assert!(
            !recs.iter().any(|x| x.f.get(Key::Cont).is_some()),
            "{state:?}: a new prompt's turn continues the interrupted one: {recs:?}"
        );
        let recs = r.tx(prompt("v2"));
        assert!(
            recs.iter()
                .any(|x| x.op == Op::Amend && x.node == t2 && vref_of(x) == Some("v2")),
            "{state:?}: {recs:?}"
        );
        r.ordered();
    }
}

#[test]
pub(crate) fn a_settles_close_is_never_dated_before_a_transcript_child() {
    for (state, late) in [
        (TxState::On, true),
        (TxState::Partial, true),
        (TxState::Partial, false),
    ] {
        let mut r = Rig::bound_at(state);
        r.see(Verdict::Busy);
        let t1 = r.turn();
        r.tx(prompt("v1"));
        r.tx(tool("c1"));
        r.now = 3_000;
        r.see(Verdict::Idle);
        let rest = r.wall();
        r.now = 3_100;
        if late {
            r.tx_at(result("c1", true), rest + 200);
            r.tx_at(tool("c2"), rest + 250);
        }
        r.wait(1_000);
        let c = ops(&r.records(), Op::Close, Lvl::Turn)
            .into_iter()
            .find(|x| x.node == t1)
            .expect("the settle closed it");
        assert_eq!(c.t, if late { rest + 250 } else { rest }, "{state:?}");
        r.ordered();
    }
}

#[test]
pub(crate) fn a_transcript_close_is_never_dated_before_a_child() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.tx(prompt("v1"));
    r.now = 2_000;
    r.see(bash());
    r.now = 2_500;
    r.see(Verdict::Busy);
    r.now = 3_000;
    let c = ops(&r.tx_at(END, WALL0 + 1_000), Op::Close, Lvl::Turn)[0].clone();
    assert_eq!(c.t, WALL0 + 2_500, "the box's close, not the row's 1 s");
    r.ordered();
}

#[test]
pub(crate) fn the_screen_busy_again_continues_a_running_vendor_turn() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    let t1 = r.turn();
    r.tx(prompt("v1"));
    r.see(Verdict::Idle);
    r.wait(1_000);
    let recs = r.see(Verdict::Busy);
    let t2 = r.turn();
    let cont = recs.iter().find(|x| x.op == Op::Amend).expect("cont=");
    assert_eq!(cont.node, t2);
    assert!(matches!(cont.f.get(Key::Cont), Some(Val::Id(id)) if id.as_str() == t1.to_string()));
    vendor(cont, Conf::Exact);
    assert_eq!(r.tr.tx_view().mapped, Some(t2));
    assert_eq!(r.tx(tool("c1"))[0].par, Some(t2));

    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.tx(prompt("v1"));
    r.see(bash());
    r.tx(END);
    r.see(Verdict::Busy);
    r.see(Verdict::Idle);
    r.wait(1_000);
    let recs = r.see(Verdict::Busy);
    assert!(!recs.iter().any(|x| x.op == Op::Amend), "{recs:?}");
    r.ordered();
}

#[test]
pub(crate) fn a_row_at_the_watermark_is_never_applied_again() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    let wall = r.wall();
    let row = r.pos(r.off + 200, wall);
    assert_eq!(r.call(prompt("v1"), row).len(), 1);
    assert!(
        r.call(tool("c1"), row).is_empty(),
        "the watermark's own row"
    );
    assert_eq!(
        r.call(
            tool("c1"),
            TxPos {
                off: row.off + 1,
                ..row
            }
        )
        .len(),
        1
    );
    r.see(Verdict::Wall(WallKind::UsageSession));
    let judge = TxEvent::Wall {
        kind: WallKind::UsageSession,
        judge: WallJudge::Confirm,
    };
    let at_the_end = TxPos {
        off: row.off + 1,
        ..row
    };
    let recs = r.call(judge, at_the_end);
    assert_eq!(
        recs.iter()
            .filter(|x| x.f.get_u(Key::Confirmed) == Some(1))
            .count(),
        1
    );
    r.ordered();
}

#[test]
pub(crate) fn the_judge_confirms_the_screens_wall_once() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.see(Verdict::Wall(WallKind::UsageSession));
    let judge = TxEvent::Wall {
        kind: WallKind::UsageSession,
        judge: WallJudge::Confirm,
    };
    let recs = r.tx(judge);
    assert_eq!(
        (recs.len(), recs[0].op, recs[0].f.get_u(Key::Confirmed)),
        (1, Op::Amend, Some(1))
    );
    vendor(&recs[0], Conf::Exact);
    assert_eq!(r.log.state().top_need().map(|n| n.conf), Some(Conf::Exact));
    assert!(r.tx(judge).is_empty(), "once");
}

#[test]
pub(crate) fn a_refuted_wall_shuts_and_the_turn_closes_at_the_next_word() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.see(Verdict::Wall(WallKind::UsageSession));
    let recs = r.tx(TxEvent::Wall {
        kind: WallKind::UsageSession,
        judge: WallJudge::Refute,
    });
    assert_eq!((recs[0].op, recs[0].out()), (Op::Close, Some("retracted")));
    vendor(&recs[0], Conf::Exact);
    assert!(
        r.log.state().top_need().is_none(),
        "the need went with the wall"
    );
    assert_eq!(r.tr.wake_at(), None);
    assert_eq!(r.tr.tx_view().refuted, Some(WallKind::UsageSession));
    for _ in 0..20 {
        r.wait(500);
        r.see(Verdict::Wall(WallKind::UsageSession));
    }
    assert!(r.tr.turn().is_some(), "the wall's settle stayed disarmed");
    r.see(Verdict::Idle);
    assert_eq!(
        r.tr.tx_view().refuted,
        None,
        "the latch lifts with the word"
    );
    let deadline = r.now + aterm_spec::wait::MUST_HAPPEN.as_millis() as u64;
    while r.tr.turn().is_some() {
        assert!(r.now < deadline, "the turn never closed");
        r.wait(100);
    }
    let c = ops(&r.records(), Op::Close, Lvl::Turn)[0].clone();
    assert_eq!(c.out(), Some("done"));
    r.ordered();
}

#[test]
pub(crate) fn a_refute_of_a_carried_wall_refines_its_turn_to_done() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    let t1 = r.turn();
    r.see(Verdict::Wall(WallKind::UsageSession));
    r.wait(1_000);
    assert_eq!(
        r.log.state().last[1].as_ref().map(|c| c.out),
        Some("wall:usage-session")
    );
    let recs = r.tx(TxEvent::Wall {
        kind: WallKind::UsageSession,
        judge: WallJudge::Refute,
    });
    let step = ops(&recs, Op::Close, Lvl::Step)[0].clone();
    assert_eq!((step.out(), step.k), (Some("retracted"), Kind::Wall));
    let amend = recs
        .iter()
        .find(|x| x.op == Op::Amend)
        .expect("the refinement");
    assert_eq!(
        (amend.node, amend.out(), amend.conf),
        (t1, Some("done"), Conf::Exact)
    );
    assert_eq!(r.log.state().last[1].as_ref().map(|c| c.out), Some("done"));
    r.ordered();
}

#[test]
pub(crate) fn a_judge_for_another_wall_or_a_partial_build_writes_nothing() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.see(Verdict::Wall(WallKind::UsageSession));
    for judge in [WallJudge::Confirm, WallJudge::Refute] {
        let other = TxEvent::Wall {
            kind: WallKind::Overloaded,
            judge,
        };
        assert!(r.tx(other).is_empty());
    }
    r.state = TxState::Partial;
    let mark = r.tx(TxEvent::Wall {
        kind: WallKind::UsageSession,
        judge: WallJudge::Refute,
    });
    assert!(
        mark.iter().all(|x| x.op == Op::Mark && x.k == Kind::Source),
        "{mark:?}"
    );
    assert!(r.tr.view(r.now).step.is_some(), "the wall stands");
}

#[test]
pub(crate) fn the_vendor_turns_own_notice_refines_the_closed_turn() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.tx(prompt("v1"));
    let wall = TxEvent::EndWall {
        kind: WallKind::UsageWeekly,
    };
    assert!(r.tx(wall).is_empty(), "the turn is open");
    r.see(Verdict::Idle);
    r.wait(1_000);
    let recs = r.tx(wall);
    assert_eq!(
        (recs[0].out(), recs[0].conf),
        (Some("wall:usage-weekly"), Conf::Exact)
    );
    assert_eq!(
        r.log.state().last[1].as_ref().map(|c| c.out),
        Some("wall:usage-weekly")
    );
}

#[test]
pub(crate) fn a_bind_seeds_past_the_history_and_says_so_once() {
    let mut r = Rig::new();
    r.see(Verdict::Busy);
    let t1 = r.turn();
    assert!(r.tx(prompt("v0")).is_empty(), "no bind, nothing read");
    let recs = r.bind(8192);
    assert_eq!(recs.len(), 1);
    let m = &recs[0];
    assert_eq!(
        (m.op, m.k, m.node, m.by, m.src),
        (Op::Mark, Kind::Source, r.run, Actor::Aterm, Src::Transcript)
    );
    assert_eq!(m.f.get_word(Key::State), Some("on"));
    assert_eq!(m.f.get_u(Key::Skipped), Some(8192));
    let at_seed = r.pos(8000, r.wall());
    assert!(r.call(prompt("v1"), at_seed).is_empty(), "history");
    assert!(
        r.tx_at(prompt("v2"), PROCESS_START - 1).is_empty(),
        "before the process"
    );
    assert!(
        r.tx_at(prompt("v3"), WALL0 - 1).is_empty(),
        "before the run opened"
    );
    let len = r.off + 400;
    assert!(r.bind(len).is_empty(), "a lapsed watch keeps the watermark");
    assert!(
        r.tx_at(prompt("v4"), WALL0)
            .iter()
            .any(|x| x.node == t1 && x.op == Op::Amend)
    );
}

#[test]
pub(crate) fn an_adoption_without_a_watermark_replays_nothing() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.tx(prompt("v1"));
    let c1 = r.tx(tool("c1"))[0].node;
    r.now = 1_000;
    let line: String =
        r.tr.carry_line(at(r.now))
            .split(' ')
            .filter(|t| !t.starts_with("tx_"))
            .collect::<Vec<_>>()
            .join(" ");
    r.now = 1_100;
    r.adopt(&line);
    assert_eq!(r.tr.tx_view().through, None);
    assert!(r.tx(prompt("v2")).is_empty(), "no watermark");
    let len = r.off;
    assert_eq!(r.bind(len)[0].f.get_u(Key::Skipped), Some(len));
    assert!(
        r.tx_at(result("c1", false), WALL0 + 1_099).is_empty(),
        "before the adoption"
    );
    let closed = r.tx(result("c1", false));
    assert_eq!((closed[0].op, closed[0].node), (Op::Close, c1));
}

#[test]
pub(crate) fn an_overflow_desyncs_the_join_until_the_next_prompt() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.tx(prompt("v1"));
    r.tx(tool("c1"));
    let recs = r.tx(TxEvent::Overflow { n: 3 });
    assert_eq!(
        (recs.len(), recs[0].k, recs[0].f.get_u(Key::Dropped)),
        (1, Kind::Source, Some(3))
    );
    assert!(r.tr.tx_view().desync);
    for ev in [tool("c2"), result("c1", false), END, TxEvent::Compact] {
        assert!(r.tx(ev).is_empty(), "{ev:?} while desynced");
    }
    assert_eq!(r.tr.tx_view().dropped, 7);
    let recs = r.tx(prompt("v2"));
    assert!(!r.tr.tx_view().desync);
    let t2 = ops(&recs, Op::Open, Lvl::Turn)[0].node;
    assert_eq!(r.tx(tool("c3"))[0].par, Some(t2));
    r.ordered();
}

#[test]
pub(crate) fn a_ninth_parallel_tool_call_is_counted_never_opened() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.tx(prompt("v1"));
    for i in 0..aterm_slog::TX_TOOLS_MAX {
        assert_eq!(r.tx(tool(&format!("c{i}"))).len(), 1);
    }
    assert!(r.tx(tool("c8")).is_empty());
    assert_eq!(r.tr.tx_view().dropped, 1);
    r.tx(result("c0", false));
    assert_eq!(r.tx(tool("c9")).len(), 1);
}

#[test]
pub(crate) fn lost_rows_are_counted_on_the_source_mark_never_as_ring_loss() {
    let mut r = Rig::bound();
    assert!(r.tx(TxEvent::Lost { n: 2 }).is_empty());
    let m = r.tx(TxEvent::Flush);
    assert_eq!(
        (m.len(), m[0].k, m[0].f.get_u(Key::Dropped)),
        (1, Kind::Source, Some(2))
    );
    assert_eq!(m[0].f.get_word(Key::State), Some("on"));
    assert!(!m[0].is_bus_class());
    assert!(r.tx(TxEvent::Flush).is_empty(), "said once");
    assert!(!r.records().iter().any(|x| x.op == Op::Gap));
    r.state = TxState::Partial;
    let m = r.tx(prompt("v1"));
    assert_eq!(
        (m[0].k, m[0].f.get_u(Key::Dropped)),
        (Kind::Source, Some(2))
    );
    assert_eq!(m[0].f.get_word(Key::State), Some("partial"));
}

#[test]
pub(crate) fn a_loss_in_a_steady_session_is_said_once_at_the_batchs_end() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.tx(prompt("v1"));
    assert!(r.tx(TxEvent::Flush).is_empty(), "nothing lost");
    for i in 0..=TX_TOOLS_MAX {
        r.tx(tool(&format!("c{i}")));
    }
    let m = r.tx(TxEvent::Flush);
    assert_eq!(
        (
            m.len(),
            m[0].k,
            m[0].f.get_u(Key::Dropped),
            m[0].f.get_word(Key::State)
        ),
        (1, Kind::Source, Some(1), Some("on")),
        "{m:?}"
    );
    assert!(r.tx(TxEvent::Flush).is_empty());
    r.tx(result("never-opened", false));
    r.tx(TxEvent::Lost { n: 2 });
    let line = r.tr.carry_line(at(r.now));
    assert!(line.contains(" tx_dr=4 "), "{line}");
    assert!(line.contains(" tx_du=1"), "{line}");
    r.now += 100;
    r.adopt(&line);
    let m = r.tx(TxEvent::Flush);
    assert_eq!(
        (m.len(), m[0].f.get_u(Key::Dropped)),
        (1, Some(4)),
        "the successor says it: {m:?}"
    );
    r.state = TxState::Off;
    r.tx(TxEvent::Lost { n: 0 });
    r.tx(TxEvent::Lost { n: 3 });
    let m = r.tx(TxEvent::Flush);
    assert_eq!(
        (
            m.len(),
            m[0].f.get_u(Key::Dropped),
            m[0].f.get_word(Key::State)
        ),
        (1, Some(7), Some("off")),
        "{m:?}"
    );
}

#[test]
pub(crate) fn the_watermark_is_keyed_on_the_files_identity() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    let other = TxPos {
        id: Some((16_777_220, 9_999)),
        ..r.pos(10_000, r.wall())
    };
    assert!(r.call(prompt("v1"), other).is_empty());
    let bind = TxEvent::Bind {
        len: 10_000,
        floor_ms: PROCESS_START,
    };
    let seeded = r.call(bind, other);
    assert_eq!(seeded[0].f.get_u(Key::Skipped), Some(10_000));
    let next = TxPos {
        off: 10_200,
        ..other
    };
    assert_eq!(r.call(prompt("v1"), next)[0].op, Op::Amend);
}

#[test]
pub(crate) fn a_partial_build_adds_and_never_overrules() {
    let mut r = Rig::new();
    r.state = TxState::Partial;
    assert_eq!(r.bind(0)[0].f.get_word(Key::State), Some("partial"));
    r.see(Verdict::Busy);
    r.tx(prompt("v1"));
    r.tx(tool("c1"));
    r.tx(result("c1", false));
    r.tx(END);
    r.tx(TxEvent::Model {
        model: Some("claude-opus-4-1"),
        effort: Some("high"),
        ver: Some("2.1.3"),
    });
    r.see(Verdict::Wall(WallKind::UsageSession));
    for judge in [WallJudge::Confirm, WallJudge::Refute] {
        r.tx(TxEvent::Wall {
            kind: WallKind::UsageSession,
            judge,
        });
    }
    r.see(Verdict::Busy);
    r.see(Verdict::Idle);
    r.wait(1_000);
    r.tx(TxEvent::EndWall {
        kind: WallKind::UsageSession,
    });
    r.tx(prompt("v2"));
    let tx: Vec<Rec> = r
        .records()
        .into_iter()
        .filter(|x| x.src == Src::Transcript && x.k != Kind::Source)
        .collect();
    assert!(tx.len() >= 5, "{tx:?}");
    for x in &tx {
        assert_eq!(x.conf, Conf::Strong, "{x:?}");
        assert!(x.node.lvl != Lvl::Turn || x.op == Op::Amend, "{x:?}");
        assert!(
            x.out().is_none() || matches!(x.k, Kind::Tool | Kind::Subagent),
            "{x:?}"
        );
        assert!(x.f.get(Key::Confirmed).is_none(), "{x:?}");
    }
    r.state = TxState::On;
    r.see(Verdict::Busy);
    let recs = r.tx(prompt("v3"));
    assert_eq!(recs[0].f.get_word(Key::State), Some("on"));
    assert_eq!(ops(&r.tx(END), Op::Close, Lvl::Turn).len(), 1);
}

#[test]
pub(crate) fn the_runs_model_is_amended_only_when_it_moves() {
    let mut r = Rig::bound();
    let ev = |effort| TxEvent::Model {
        model: Some("claude-opus-4-1"),
        effort: Some(effort),
        ver: Some("2.1.3"),
    };
    let a = r.tx(ev("high"));
    assert_eq!(
        (a[0].op, a[0].node, a[0].k),
        (Op::Amend, r.run, Kind::Agent)
    );
    assert_eq!(a[0].f.get_str(Key::Model), Some("claude-opus-4-1"));
    assert!(r.tx(ev("high")).is_empty());
    let b = r.tx(ev("low"));
    assert_eq!(
        (b[0].f.get_str(Key::Effort), b[0].f.get(Key::Model)),
        (Some("low"), None)
    );
}

#[test]
pub(crate) fn a_compaction_is_an_instant_step() {
    let mut r = Rig::bound();
    let recs = r.tx(TxEvent::Compact);
    assert_eq!(
        (recs[0].op, recs[0].k, recs[0].par),
        (Op::Open, Kind::Compact, Some(r.run))
    );
    assert_eq!(
        (recs[1].op, recs[1].out(), recs[1].f.get_u(Key::DurMs)),
        (Op::Close, Some("ok"), Some(0))
    );
    r.see(Verdict::Busy);
    let t1 = r.turn();
    assert_eq!(r.tx(TxEvent::Compact)[0].par, Some(t1));
}

#[test]
pub(crate) fn the_carry_keeps_the_join_and_the_successor_rebuilds_its_steps() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    let t1 = r.turn();
    r.tx(prompt("v1"));
    let c1 = r.tx(tool("c1"))[0].node;
    r.tx(tool("c2"));
    let last = r.pos(r.off, r.wall());
    let line = r.tr.carry_line(at(r.now));
    for tok in [" tx_fp=", " tx_off=", " tx_id=", " tx_v=v1", " tx_st=on"] {
        assert!(line.contains(tok), "{tok} in {line}");
    }
    assert!(line.contains(&format!(" tx_vt={t1}")));
    assert!(
        !line.split(' ').any(|t| t
            .split_once('=')
            .is_some_and(|(_, v)| v == "c1" || v == "c2")),
        "no tool map: {line}"
    );
    r.now = 500;
    r.adopt(&line);
    let tv = r.tr.tx_view();
    assert_eq!(
        (tv.vref, tv.mapped, tv.floor_ms),
        (Some(v("v1")), Some(t1), WALL0 + 500)
    );
    assert!(r.call(tool("c3"), last).is_empty(), "at the watermark");
    assert!(
        r.tx_at(tool("c4"), WALL0 + 499).is_empty(),
        "older than the adoption"
    );
    let closed = r.tx(result("c1", false));
    assert_eq!((closed[0].op, closed[0].node), (Op::Close, c1));
    assert_eq!(r.tr.tx_view().tools, 1, "c2 rebuilt, c1 resulted");

    let bad = line.replace(" tx_off=", " tx_off=x");
    let b = TurnTracker::from_carry_line(&bad, at(600)).expect("the line still reads");
    assert_eq!((b.tx_view().through, b.tx_view().vref), (None, None));
}

#[test]
fn the_worst_tracker_line_fits_a_carry_line() {
    let id40 = "a".repeat(ShortId::MAX);
    let max = u64::MAX;
    let detail = "%E9".repeat(16);
    let worst = format!(
        "tracker run=r4294967295 turn=t4294967295 word=wall:usage-session interrupted=1 \
         step=p4294967295 step_under=1 step_what=wall step_wall=usage-session step_detail={detail} \
         settle=wall:usage-session settle_wall={max} \
         ledger={max} ledger_by={id40} ledger_wall={max} ledger_cause={id40} \
         input_by={id40} input_wall={max} input_cause={id40} \
         tx_fp={max:016x} tx_off={max} tx_id={max:x}:{max:x} tx_v={id40} tx_vt=t4294967295 \
         tx_ve={max} tx_vp={max} tx_ref=usage-session tx_st=partial tx_ds=1 tx_dr={max} \
         tx_du=1"
    );
    let now = at(10_000);
    let tr = TurnTracker::from_carry_line(&worst, now).expect("the worst line reads");
    let back = tr.carry_line(now);
    assert!(
        "host ".len() + back.len() <= CARRY_MAX_LINE,
        "{} bytes: {back}",
        back.len()
    );
    for tok in [
        "step_detail=",
        "ledger_cause=",
        "input_cause=",
        "tx_fp=",
        "tx_id=",
        "tx_v=",
        "tx_vt=",
        "tx_ve=",
        "tx_vp=",
        "tx_ref=",
        "tx_st=",
        "tx_ds=1",
        "tx_dr=",
        "tx_du=1",
    ] {
        assert!(back.contains(tok), "{tok} lost: {back}");
    }
}

#[test]
pub(crate) fn a_closed_run_takes_no_transcript_row() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.tx(prompt("v1"));
    let c1 = r.tx(tool("c1"))[0].node;
    let close = aterm_slog::Close::new(
        aterm_slog::Out::Run(aterm_slog::RunOut::Gone),
        Src::Status,
        Conf::Strong,
    );
    r.log
        .close(r.run, close, at(r.now))
        .expect("the run closes");
    let gone = r
        .records()
        .into_iter()
        .find(|x| x.op == Op::Close && x.node == c1);
    assert_eq!(gone.and_then(|x| x.out()), Some("gone"));
    assert!(r.tx(result("c1", false)).is_empty());
    assert!(r.tx(prompt("v2")).is_empty());
    r.ordered();
}

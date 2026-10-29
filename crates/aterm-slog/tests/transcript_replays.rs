// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Transcript join replays across [`TxEvent`], [`Log`], and [`TurnTracker`]. Each one
//! [`crate::seq_twins::TX_TWINS`] names runs there, against its `.seq` file; the rest are tests.

use aterm_slog::{
    ATTRIBUTION_MS, Actor, BoxWord, CARRY_MAX_LINE, Claimed, Conf, Key, Kind, Log, Lvl, Mark, Node,
    Op, Rec, ShortId, Src, TX_TOOLS_MAX, TurnTracker, TxEvent, TxPos, TxState, Val, Verdict,
    WallJudge, WallKind,
};

use crate::conformance_transcript::{carried_unmapped, project_parts};
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

    /// The program's own claim (`TurnTracker::claim`), which no `.seq` twin replays.
    fn claim(&mut self, c: Claimed) -> Vec<Rec> {
        let seq0 = self.log.last_seq();
        self.tr.claim(&mut self.log, c, at(self.now));
        self.since(seq0)
    }

    /// An engine edge no status sample showed (`TurnTracker::claim_edge`), at its own batch time
    /// `at_ms`, with the run's bucket answering `may_open`; no `.seq` twin replays it either.
    fn edge(&mut self, c: Claimed, at_ms: u64, may_open: bool) -> Vec<Rec> {
        let seq0 = self.log.last_seq();
        self.tr
            .claim_edge(&mut self.log, c, at(at_ms), &mut || may_open);
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

pub(crate) fn a_prompt_read_before_the_screens_turn_names_it() {
    // A successor that adopted an older build's line holding an unmapped vendor turn
    // (`tx_v`, `tx_vp`, no `tx_vt`) names the screen's turn that opens within the window.
    let mut r = Rig::bound_at(TxState::Partial);
    r.now = 10_000;
    let line = carried_unmapped(&r.tr.carry_line(at(r.now)), v("vlead"), r.wall());
    r.adopt(&line);
    assert_eq!(
        (r.tr.tx_view().vref, r.tr.tx_view().mapped),
        (Some(v("vlead")), None)
    );
    r.now += 2_000;
    let recs = r.see(Verdict::Busy);
    let t = r.turn();
    assert!(
        recs.iter()
            .any(|x| x.op == Op::Amend && x.node == t && vref_of(x) == Some("vlead")),
        "{recs:?}"
    );
    assert!(
        r.tx(tool("c1"))
            .iter()
            .any(|x| x.op == Op::Open && x.par == Some(t))
    );
    assert_eq!(r.tr.tx_view().dropped, 0);
    r.ordered();

    // Fix E1: a partial prompt with no turn open opens its own turn, however long before
    // the screen sees the agent busy; busy opens nothing and the call opens under it.
    let mut r = Rig::bound_at(TxState::Partial);
    r.now = 10_000;
    let recs = r.tx(prompt("vlead"));
    let opened = ops(&recs, Op::Open, Lvl::Turn);
    assert_eq!(opened.len(), 1, "{recs:?}");
    assert_eq!(
        (opened[0].src, opened[0].conf, vref_of(&opened[0])),
        (Src::Transcript, Conf::Strong, Some("vlead"))
    );
    let t = opened[0].node;
    r.now += ATTRIBUTION_MS + 1_000;
    assert!(
        ops(&r.see(Verdict::Busy), Op::Open, Lvl::Turn).is_empty(),
        "busy opens no second turn"
    );
    assert_eq!(r.tr.turn(), Some(t));
    assert!(
        r.tx(tool("c1"))
            .iter()
            .any(|x| x.op == Op::Open && x.par == Some(t))
    );
    assert_eq!(r.tr.tx_view().dropped, 0);
    r.ordered();
}

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

pub(crate) fn a_tool_use_for_a_closed_turn_never_opens_a_step() {
    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    r.tx(prompt("v1"));
    r.see(Verdict::Idle);
    r.wait(1_000);
    assert!(r.tr.turn().is_none());
    assert!(r.tx(tool("c1")).is_empty());
    assert_eq!(r.tr.tx_view().dropped, 1);
    assert!(!r.records().iter().any(|x| x.k == Kind::Tool));
}

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
fn a_late_end_follows_every_continuation_to_its_dated_segment() {
    for ended in [1_500, 4_000, 6_500, 9_000] {
        for close_last in [false, true] {
            let mut r = Rig::bound();
            r.see(Verdict::Busy);
            let mut turns = vec![r.turn()];
            r.tx(prompt("v1"));
            for rest in [1_500, 4_000, 6_500] {
                r.now = rest;
                r.see(Verdict::Idle);
                r.wait(1_000);
                r.now += 300;
                let recs = r.see(Verdict::Busy);
                let previous = *turns.last().expect("the previous screen turn");
                let previous_id = previous.to_string();
                let next = r.turn();
                assert!(recs.iter().any(|rec| {
                    rec.node == next && rec.f.get_str(Key::Cont) == Some(previous_id.as_str())
                }));
                turns.push(next);
            }
            r.now = 9_000;
            if close_last {
                r.see(Verdict::Idle);
                r.wait(1_000);
            }
            let target = turns[match ended {
                1_500 => 0,
                4_000 => 1,
                6_500 => 2,
                _ => 3,
            }];
            let recs = r.tx_at(END, WALL0 + ended);
            let numbers = recs
                .iter()
                .find(|rec| rec.f.get(Key::VdurMs).is_some())
                .expect("the vendor totals are attributed");
            assert_eq!(numbers.node, target, "end={ended}, closed={close_last}");
            assert_eq!(numbers.f.get_u(Key::TokIn), Some(900));
            assert_eq!(r.tr.tx_view().mapped, Some(target));
            assert_eq!(r.tr.tx_view().dropped, 0);
            let closes = ops(&recs, Op::Close, Lvl::Turn);
            if ended == 9_000 && !close_last {
                assert_eq!(closes.len(), 1);
                assert_eq!(closes[0].node, target);
                assert_eq!(r.tr.turn(), None);
            } else {
                assert!(closes.is_empty(), "a prior end closes no later segment");
                assert_eq!(r.tr.turn(), (!close_last).then_some(turns[3]));
            }
            r.ordered();
        }
    }

    let mut r = Rig::bound();
    r.see(Verdict::Busy);
    let first = r.turn();
    r.tx(prompt("v1"));
    for rest in [1_500, 4_000, 6_500] {
        r.now = rest;
        r.see(Verdict::Idle);
        r.wait(1_000);
        r.now += 300;
        r.see(Verdict::Busy);
    }
    let current = r.turn();
    r.log.set_byte_budget(0);
    assert!(
        !r.log
            .window()
            .any(|rec| rec.node == first && rec.op == Op::Open)
    );
    let recs = r.tx_at(END, WALL0 + 1_500);
    assert!(!recs.iter().any(|rec| rec.f.get(Key::VdurMs).is_some()));
    assert!(ops(&recs, Op::Close, Lvl::Turn).is_empty());
    assert_eq!(r.tr.turn(), Some(current));
    assert_eq!(r.tr.tx_view().dropped, 1, "unknown ancestry is counted");
}

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
    let t2 = ops(&r.tx(prompt("v2")), Op::Open, Lvl::Turn);
    assert_eq!(
        (t2.len(), t2[0].src, vref_of(&t2[0])),
        (1, Src::Transcript, Some("v2")),
        "fix E1: the turn the screen missed: {t2:?}"
    );
    let records = r.records();
    let own: Vec<Node> = records
        .iter()
        .filter(|x| x.op == Op::Open && x.node.lvl == Lvl::Turn && x.src == Src::Transcript)
        .map(|x| x.node)
        .collect();
    let tx: Vec<&Rec> = records
        .iter()
        .filter(|x| x.src == Src::Transcript && x.k != Kind::Source)
        .collect();
    assert!(tx.len() >= 6, "{tx:?}");
    for x in &tx {
        assert_eq!(x.conf, Conf::Strong, "{x:?}");
        assert!(
            x.node.lvl != Lvl::Turn || x.op == Op::Amend || own.contains(&x.node),
            "a turn the screen opened is only amended: {x:?}"
        );
        assert!(
            x.out().is_none()
                || matches!(x.k, Kind::Tool | Kind::Subagent)
                || (x.op == Op::Close && own.contains(&x.node)),
            "an outcome only on its own step or turn, never an out= amend on a turn: {x:?}"
        );
        assert!(x.f.get(Key::Confirmed).is_none(), "{x:?}");
    }
    let t1 = ops(&records, Op::Close, Lvl::Turn);
    assert_eq!(
        (t1.len(), t1[0].src, t1[0].out()),
        (1, Src::Screen, Some("done")),
        "the screen closes its own turn"
    );
    r.state = TxState::On;
    r.see(Verdict::Busy);
    let recs = r.tx(prompt("v3"));
    assert_eq!(recs[0].f.get_word(Key::State), Some("on"));
    let closed = ops(&recs, Op::Close, Lvl::Turn);
    assert_eq!(
        (closed.len(), closed[0].node, closed[0].out()),
        (1, t2[0].node, Some("superseded"))
    );
    assert_eq!(ops(&r.tx(END), Op::Close, Lvl::Turn).len(), 1);
}

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

/// THE CLAIM'S TURNS: a run whose screen reads nothing and whose transcript steps no turn (none
/// bound, or latched off) has a turn for each working-to-rest claim, opened and closed at
/// `src=engine conf=claim` with its duration (`out=error` for a failed rest), a second working
/// claim inside one keeping it (the host maps `blocked` to working: aterm-gui's
/// `a_codex_alabs_blocked_claim_keeps_its_turn_and_an_error_closes_it`); a screen word that opens
/// no turn (a codex-alab's `unknown`, or `idle`) leaves the claims their turns. NEGATIVE CONTROL:
/// the same claims into a run whose screen opened a turn open and close nothing.
#[test]
pub(crate) fn a_claimed_turn_runs_from_working_to_rest_where_nothing_else_opens_one() {
    let mut r = Rig::new();
    let opened = r.claim(Claimed::Working);
    let t1 = r.turn();
    let o = ops(&opened, Op::Open, Lvl::Turn);
    assert_eq!(o.len(), 1, "{opened:?}");
    assert_eq!(
        (o[0].node, o[0].src, o[0].conf),
        (t1, Src::Engine, Conf::Claim)
    );
    assert_eq!(o[0].par, Some(r.run));
    r.now = 1_000;
    assert!(
        r.claim(Claimed::Working).is_empty(),
        "a turn runs: one turn"
    );
    r.now = 4_000;
    let closed = r.claim(Claimed::Rested);
    let c = ops(&closed, Op::Close, Lvl::Turn);
    assert_eq!(c.len(), 1, "{closed:?}");
    assert_eq!(
        (c[0].node, c[0].out(), c[0].src, c[0].conf, c[0].t),
        (t1, Some("done"), Src::Engine, Conf::Claim, WALL0 + 4_000)
    );
    assert_eq!(r.tr.turn(), None);
    r.now = 4_500;
    assert!(r.claim(Claimed::Rested).is_empty(), "no turn to close");
    r.now = 6_000;
    r.claim(Claimed::Working);
    let t2 = r.turn();
    assert_ne!(t2, t1);
    r.now = 7_000;
    let failed = ops(&r.claim(Claimed::Failed), Op::Close, Lvl::Turn);
    assert_eq!((failed[0].node, failed[0].out()), (t2, Some("error")));
    let mut off = Rig::bound_at(TxState::Off);
    off.claim(Claimed::Working);
    assert!(
        off.tr.turn().is_some(),
        "a transcript latched off steps no turn"
    );
    for word in [Verdict::Unknown, Verdict::Idle] {
        let mut resting = Rig::new();
        resting.see(word);
        resting.claim(Claimed::Working);
        assert!(
            resting.tr.turn().is_some(),
            "a screen word that opens no turn ({word:?}) leaves the claim its turns"
        );
    }
    let mut screen = Rig::new();
    screen.see(Verdict::Busy);
    let own = screen.turn();
    for c in [Claimed::Rested, Claimed::Failed, Claimed::Working] {
        assert!(
            screen.claim(c).is_empty(),
            "NEGATIVE CONTROL: a screen that opened a turn keeps the turns ({c:?})"
        );
    }
    assert_eq!(screen.tr.turn(), Some(own));
    r.ordered();
}

/// NEVER TWO TURNS FOR ONE SPAN: a transcript read `partial` adds to the claim's turn (its prompt
/// names it, its calls hang from it, its end gives it a duration and tokens) and never closes it,
/// the claim's rest does; one read `on` opens and closes its own turns and the claims open none,
/// and a claim's turn open when such a transcript binds is the one its prompt names. A partial
/// prompt read before the claim's `working` opens its own turn (fix E1), which that `working` takes
/// as the turn running, so the span is still one turn.
/// NEGATIVE CONTROL: the `on` transcript's own turn, with claims, is its only turn.
#[test]
pub(crate) fn a_partial_transcript_fills_the_claims_turn_and_an_exact_one_keeps_its_own() {
    let mut r = Rig::bound_at(TxState::Partial);
    r.claim(Claimed::Working);
    let t1 = r.turn();
    r.now = 100;
    let named = r.tx(prompt("v1"));
    assert_eq!(
        (named[named.len() - 1].op, named[named.len() - 1].node),
        (Op::Amend, t1)
    );
    assert_eq!(vref_of(&named[named.len() - 1]), Some("v1"));
    vendor(&named[named.len() - 1], Conf::Strong);
    r.now = 200;
    let call = r.tx(tool("c1"));
    assert_eq!(call[0].par, Some(t1));
    vendor(&call[0], Conf::Strong);
    r.now = 300;
    r.tx(result("c1", false));
    r.now = 900;
    let end = r.tx(END);
    assert!(
        ops(&end, Op::Close, Lvl::Turn).is_empty(),
        "partial never closes"
    );
    assert_eq!(end[0].f.get_u(Key::VdurMs), Some(4200));
    assert_eq!(end[0].f.get_u(Key::TokIn), Some(900));
    r.now = 1_000;
    let rest = ops(&r.claim(Claimed::Rested), Op::Close, Lvl::Turn);
    assert_eq!((rest[0].node, rest[0].src), (t1, Src::Engine));
    assert_eq!(ops(&r.records(), Op::Open, Lvl::Turn).len(), 1, "one turn");
    r.ordered();

    let mut first = Rig::bound_at(TxState::Partial);
    first.now = 100;
    let own = ops(&first.tx(prompt("v1")), Op::Open, Lvl::Turn);
    assert_eq!(
        own.iter().map(|o| (o.src, o.conf)).collect::<Vec<_>>(),
        [(Src::Transcript, Conf::Strong)],
        "the turn the claims have not opened yet is the partial prompt's own"
    );
    let t1 = first.turn();
    first.now = 150;
    assert!(
        ops(&first.claim(Claimed::Working), Op::Open, Lvl::Turn).is_empty(),
        "the claim's working takes the turn running"
    );
    assert_eq!(first.tr.turn(), Some(t1));
    first.now = 900;
    let closed = ops(&first.tx(END), Op::Close, Lvl::Turn);
    assert_eq!(
        closed.iter().map(|c| (c.node, c.src)).collect::<Vec<_>>(),
        [(t1, Src::Transcript)],
        "a partial end closes the turn it opened"
    );
    first.now = 1_000;
    assert!(first.claim(Claimed::Rested).is_empty(), "no turn to close");
    assert_eq!(
        ops(&first.records(), Op::Open, Lvl::Turn).len(),
        1,
        "one turn"
    );
    first.ordered();

    let mut exact = Rig::bound();
    assert!(
        exact.claim(Claimed::Working).is_empty(),
        "the transcript opens"
    );
    exact.now = 100;
    let own = ops(&exact.tx(prompt("v1")), Op::Open, Lvl::Turn);
    assert_eq!(
        (own[0].src, vref_of(&own[0])),
        (Src::Transcript, Some("v1"))
    );
    exact.now = 900;
    let closed = ops(&exact.tx(END), Op::Close, Lvl::Turn);
    assert_eq!(closed[0].src, Src::Transcript);
    exact.now = 1_000;
    assert!(exact.claim(Claimed::Rested).is_empty());
    exact.now = 2_000;
    assert!(exact.claim(Claimed::Working).is_empty());
    assert_eq!(
        ops(&exact.records(), Op::Open, Lvl::Turn).len(),
        1,
        "NEGATIVE CONTROL: the transcript's turn alone"
    );
    exact.ordered();

    let mut late = Rig::new();
    late.claim(Claimed::Working);
    let t1 = late.turn();
    late.state = TxState::On;
    late.bind(0);
    late.now = 100;
    let named = late.tx(prompt("v1"));
    assert!(ops(&named, Op::Open, Lvl::Turn).is_empty(), "{named:?}");
    assert_eq!(late.tr.tx_view().mapped, Some(t1));
    late.now = 900;
    let closed = ops(&late.tx(END), Op::Close, Lvl::Turn);
    assert_eq!((closed[0].node, closed[0].src), (t1, Src::Transcript));
    late.now = 1_000;
    assert!(late.claim(Claimed::Rested).is_empty());
    assert_eq!(ops(&late.records(), Op::Open, Lvl::Turn).len(), 1);
    late.ordered();
}

/// A TRANSCRIPT READ LATE NAMES THE CLAIMS' CLOSED TURNS: two turns the claims opened and closed
/// before any transcript bound (a self-claim proven late), then a transcript bound `state=on`
/// reading both vendor turns from its history. Each prompt names the claim's closed turn opened
/// nearest its row, and its end gives that turn its duration and tokens, so the run holds two
/// turns for the two spans, not four. NEGATIVE CONTROL: a history prompt no claim's turn opened
/// within [`ATTRIBUTION_MS`] of opens the transcript's own turn.
#[test]
pub(crate) fn a_late_exact_transcript_names_the_claims_closed_turns_and_opens_no_second() {
    let mut r = Rig::new();
    let mut claimed = Vec::new();
    for (from, to) in [(10_000, 20_000), (30_000, 40_000)] {
        r.now = from;
        r.claim(Claimed::Working);
        claimed.push(r.turn());
        r.now = to;
        r.claim(Claimed::Rested);
    }
    r.now = 55_000;
    r.state = TxState::On;
    r.bind(0);
    for ((vref, from, to), node) in [("v1", 10_100, 19_900), ("v2", 30_100, 39_900)]
        .into_iter()
        .zip(claimed)
    {
        let named = r.tx_at(prompt(vref), WALL0 + from);
        assert!(ops(&named, Op::Open, Lvl::Turn).is_empty(), "{named:?}");
        let amend = named
            .iter()
            .find(|x| x.op == Op::Amend)
            .expect("the prompt names a turn");
        assert_eq!((amend.node, vref_of(amend)), (node, Some(vref)));
        vendor(amend, Conf::Exact);
        let end = r.tx_at(END, WALL0 + to);
        assert!(ops(&end, Op::Close, Lvl::Turn).is_empty(), "{end:?}");
        assert_eq!(
            (end[0].node, end[0].f.get_u(Key::VdurMs)),
            (node, Some(4200))
        );
    }
    assert_eq!(
        ops(&r.records(), Op::Open, Lvl::Turn).len(),
        2,
        "two spans, two turns"
    );
    r.now = 70_000;
    let own = ops(&r.tx_at(prompt("v3"), WALL0 + 47_000), Op::Open, Lvl::Turn);
    assert!(
        own.len() == 1 && own[0].src == Src::Transcript && vref_of(&own[0]) == Some("v3"),
        "NEGATIVE CONTROL: a vendor turn no claim's turn holds is the transcript's own: {own:?}"
    );
    r.ordered();
}

/// A CLAIM'S REST ENDS ITS TURN: the program rests and claims its next turn before the
/// transcript's end row of the first is read (the tap reads at most once a second). The next
/// turn continues nothing (no `cont=`), the end row read late gives the first turn its duration,
/// and the next prompt names the next turn. NEGATIVE CONTROL: a screen's turn closed and opened
/// in the same order continues the vendor turn (`cont=`), since a screen can close one mid-turn.
#[test]
pub(crate) fn a_claimed_turn_after_a_rest_continues_no_vendor_turn() {
    let mut r = Rig::bound_at(TxState::Partial);
    r.claim(Claimed::Working);
    let t1 = r.turn();
    r.now = 100;
    r.tx(prompt("v1"));
    r.now = 2_000;
    r.claim(Claimed::Rested);
    r.now = 2_300;
    let next = r.claim(Claimed::Working);
    let t2 = r.turn();
    assert_ne!(t2, t1);
    assert!(
        next.iter().all(|x| x.f.get(Key::Cont).is_none()),
        "{next:?}"
    );
    r.now = 2_400;
    let end = r.tx_at(END, WALL0 + 1_950);
    assert_eq!((end[0].node, end[0].f.get_u(Key::VdurMs)), (t1, Some(4200)));
    r.now = 2_500;
    let named = r.tx(prompt("v2"));
    assert_eq!(
        (named[0].op, named[0].node, vref_of(&named[0])),
        (Op::Amend, t2, Some("v2"))
    );
    assert!(
        r.records().iter().all(|x| x.f.get(Key::Cont).is_none()),
        "{:?}",
        r.records()
    );
    r.ordered();

    let mut s = Rig::bound_at(TxState::Partial);
    s.see(Verdict::Busy);
    let s1 = s.turn().to_string();
    s.tx(prompt("v1"));
    s.see(Verdict::Idle);
    s.wait(1_000);
    let recs = s.see(Verdict::Busy);
    assert!(
        recs.iter()
            .any(|x| x.op == Op::Amend && x.f.get_str(Key::Cont) == Some(s1.as_str())),
        "NEGATIVE CONTROL: the screen's next turn continues the vendor turn: {recs:?}"
    );
}

/// A STOPPED READER HANDS THE TURNS BACK TO THE CLAIMS: while a transcript read `state=on` holds
/// the run's turns the claims stand aside; once its reader stops (`TurnTracker::tx_stopped`: the
/// tap dropped, or its file gone) they open and close turns again, until the next transcript
/// fact, after which the gap's prompt names the claim's turn the gap left and the claims stand
/// aside again. NEGATIVE CONTROL: before the stop, the same claims open nothing.
#[test]
pub(crate) fn a_stopped_exact_reader_hands_the_turns_back_to_the_claims() {
    let mut r = Rig::bound();
    r.now = 100;
    r.tx(prompt("v1"));
    r.now = 900;
    r.tx(END);
    r.now = 2_000;
    for c in [Claimed::Working, Claimed::Rested] {
        assert!(
            r.claim(c).is_empty(),
            "NEGATIVE CONTROL: the reader reads, and its turns are the run's ({c:?})"
        );
    }
    r.tr.tx_stopped();
    r.now = 3_000;
    let opened = ops(&r.claim(Claimed::Working), Op::Open, Lvl::Turn);
    assert_eq!(
        (opened.len(), opened[0].src, opened[0].conf),
        (1, Src::Engine, Conf::Claim),
        "{opened:?}"
    );
    let t2 = opened[0].node;
    r.now = 5_000;
    let closed = ops(&r.claim(Claimed::Rested), Op::Close, Lvl::Turn);
    assert_eq!((closed[0].node, closed[0].src), (t2, Src::Engine));
    r.now = 8_000;
    let off = r.off;
    r.bind(off);
    let named = r.tx_at(prompt("v2"), WALL0 + 3_050);
    assert!(ops(&named, Op::Open, Lvl::Turn).is_empty(), "{named:?}");
    assert_eq!(r.tr.tx_view().mapped, Some(t2));
    r.tx_at(END, WALL0 + 4_900);
    r.now = 9_000;
    assert!(
        r.claim(Claimed::Working).is_empty(),
        "read again: the claims stand aside"
    );
    assert_eq!(ops(&r.records(), Op::Open, Lvl::Turn).len(), 2);
    r.ordered();
}

/// AN ESC ON A CLAIM'S TURN: the program's claim has no interrupted state, so a transcript read
/// `state=partial` is that turn's witness: its interrupt read while the claim's turn runs closes
/// the turn `out=interrupted` at the rest. One read after a rest closed the turn writes nothing:
/// only an exact source refines a closed turn's outcome, so it stays `done`. NEGATIVE CONTROL: a
/// screen's turn keeps the screen's outcome, as an unmeasured Claude Code's does.
#[test]
pub(crate) fn a_partial_interrupt_is_the_claims_turns_witness_of_an_esc() {
    let mut r = Rig::bound_at(TxState::Partial);
    r.claim(Claimed::Working);
    let t1 = r.turn();
    r.now = 100;
    r.tx(prompt("v1"));
    r.now = 600;
    r.tx(TxEvent::Interrupt);
    r.now = 700;
    let c = ops(&r.claim(Claimed::Rested), Op::Close, Lvl::Turn);
    assert_eq!(
        (c[0].node, c[0].out(), c[0].src),
        (t1, Some("interrupted"), Src::Engine)
    );
    r.now = 2_000;
    r.claim(Claimed::Working);
    let t2 = r.turn();
    r.now = 2_100;
    r.tx(prompt("v2"));
    r.now = 2_600;
    let c = ops(&r.claim(Claimed::Rested), Op::Close, Lvl::Turn);
    assert_eq!((c[0].node, c[0].out()), (t2, Some("done")));
    r.now = 3_000;
    let late = r.tx_at(TxEvent::Interrupt, WALL0 + 2_550);
    assert!(late.iter().all(|x| x.out().is_none()), "{late:?}");
    assert_eq!(
        r.log.state().last[1].as_ref().map(|c| (c.node, c.out)),
        Some((t2, "done"))
    );
    r.now = 4_000;
    let opened = ops(&r.tx(prompt("v3")), Op::Open, Lvl::Turn);
    let t3 = opened[0].node;
    r.now = 4_100;
    assert!(r.claim(Claimed::Working).is_empty());
    r.now = 4_200;
    r.tx(TxEvent::Interrupt);
    r.now = 4_300;
    let c = ops(&r.claim(Claimed::Rested), Op::Close, Lvl::Turn);
    assert_eq!(
        (c[0].node, c[0].out(), c[0].src),
        (t3, Some("interrupted"), Src::Engine),
        "a partial prompt opened before working (fix E1) still closes interrupted at rest"
    );
    r.ordered();

    let mut lost = Rig::bound_at(TxState::Partial);
    lost.now = 100;
    let opened = ops(&lost.tx(prompt("v1")), Op::Open, Lvl::Turn);
    lost.now = 200;
    lost.tx(TxEvent::Interrupt);
    lost.now = 300;
    let c = ops(&lost.claim(Claimed::Rested), Op::Close, Lvl::Turn);
    assert_eq!(
        c.iter()
            .map(|c| (c.node, c.out(), c.src))
            .collect::<Vec<_>>(),
        [(opened[0].node, Some("interrupted"), Src::Engine)],
        "a partial prompt no beginning took (its held working replaced) closes at its rest"
    );
    lost.now = 1_000;
    let next = ops(&lost.edge(Claimed::Working, 400, true), Op::Open, Lvl::Turn);
    assert_eq!(
        next.len(),
        1,
        "the next beginning takes no closed turn: {next:?}"
    );
    lost.edge(Claimed::Rested, 450, true);
    lost.ordered();

    let mut exact = Rig::bound();
    exact.now = 100;
    exact.tx(prompt("v1"));
    let te = exact.turn();
    exact.now = 200;
    exact.tx(TxEvent::Interrupt);
    exact.now = 300;
    let c = ops(&exact.claim(Claimed::Rested), Op::Close, Lvl::Turn);
    assert_eq!(
        (c[0].node, c[0].out(), c[0].src),
        (te, Some("interrupted"), Src::Engine),
        "an exact interrupt emits no TurnEnd, so the claim's rest closes the turn interrupted"
    );
    exact.ordered();

    let mut s = Rig::bound_at(TxState::Partial);
    s.see(Verdict::Busy);
    let own = s.turn();
    s.tx(prompt("v1"));
    s.tx(TxEvent::Interrupt);
    s.see(Verdict::Idle);
    s.wait(1_000);
    assert_eq!(
        s.log.state().last[1].as_ref().map(|c| (c.node, c.out)),
        Some((own, "done")),
        "NEGATIVE CONTROL: {:?}",
        s.records()
    );
}

/// The turns the log opened, oldest first.
fn turn_opens(r: &Rig) -> Vec<Rec> {
    ops(&r.records(), Op::Open, Lvl::Turn)
}

/// AN EDGE NO SAMPLE SHOWED (`TurnTracker::claim_edge`): a working-to-rest the host feeds from the
/// engine's record of it, after both edges have passed, is one claimed turn (`src=engine
/// conf=claim`) opened and closed at the engine's own times, however late the feed, with the
/// duration between them (`out=error` for a failed end). NEGATIVE CONTROLS: a screen that opened
/// a turn, and a transcript read `state=on`, keep the run's turns, so the same edges write
/// nothing.
#[test]
pub(crate) fn an_edge_no_sample_showed_is_one_claimed_turn_at_its_own_times() {
    let mut r = Rig::new();
    r.now = 5_000;
    let opened = ops(&r.edge(Claimed::Working, 1_000, true), Op::Open, Lvl::Turn);
    assert_eq!(
        opened
            .iter()
            .map(|o| (o.t, o.src, o.conf, o.par))
            .collect::<Vec<_>>(),
        [(WALL0 + 1_000, Src::Engine, Conf::Claim, Some(r.run))],
    );
    let t1 = r.turn();
    let closed = ops(&r.edge(Claimed::Rested, 1_017, true), Op::Close, Lvl::Turn);
    assert_eq!(
        closed
            .iter()
            .map(|c| (c.node, c.t, c.f.get_u(Key::DurMs), c.out(), c.src, c.conf))
            .collect::<Vec<_>>(),
        [(
            t1,
            WALL0 + 1_017,
            Some(17),
            Some("done"),
            Src::Engine,
            Conf::Claim
        )],
    );
    r.edge(Claimed::Working, 2_000, true);
    let t2 = r.turn();
    let failed = ops(&r.edge(Claimed::Failed, 2_030, true), Op::Close, Lvl::Turn);
    assert_eq!(
        (failed[0].node, failed[0].t, failed[0].out()),
        (t2, WALL0 + 2_030, Some("error"))
    );
    assert_eq!(turn_opens(&r).len(), 2, "one turn for each span");
    r.ordered();

    let mut screen = Rig::new();
    screen.see(Verdict::Busy);
    let own = screen.turn();
    screen.now = 5_000;
    let mut exact = Rig::bound();
    exact.now = 5_000;
    for (c, ms) in [(Claimed::Working, 1_000), (Claimed::Rested, 1_017)] {
        assert!(
            screen.edge(c, ms, true).is_empty(),
            "NEGATIVE CONTROL: a screen that opened a turn keeps the turns ({c:?})"
        );
        assert!(
            exact.edge(c, ms, true).is_empty(),
            "NEGATIVE CONTROL: a transcript read exactly keeps the turns ({c:?})"
        );
    }
    assert_eq!(screen.tr.turn(), Some(own));
    assert!(turn_opens(&exact).is_empty(), "{:?}", exact.records());
    screen.ordered();
    exact.ordered();
}

/// A STOPPED EXACT READER KEEPS THE TURNS IT READ: the reader read a vendor turn's prompt and end,
/// then stopped before the host fed the engine's edges of that turn, so the edges dated no later
/// than the newest row it applied write nothing. NEGATIVE CONTROLS: an edge dated after that row
/// opens a claimed turn, and an end row lost to a desync is no row applied, so the engine's end
/// closes the turn the transcript left open.
#[test]
pub(crate) fn a_stopped_exact_reader_keeps_the_turns_it_read() {
    let mut r = Rig::bound();
    r.now = 100;
    r.tx(prompt("v1"));
    r.now = 900;
    r.tx(END);
    r.tr.tx_stopped();
    r.now = 1_000;
    for (c, ms) in [(Claimed::Working, 105), (Claimed::Rested, 895)] {
        let recs = r.edge(c, ms, true);
        assert!(recs.is_empty(), "{c:?}: {recs:?}");
    }
    assert_eq!(turn_opens(&r).len(), 1, "the transcript's turn alone");
    let opened = ops(&r.edge(Claimed::Working, 950, true), Op::Open, Lvl::Turn);
    assert_eq!(
        opened
            .iter()
            .map(|o| (o.t, o.src, o.conf))
            .collect::<Vec<_>>(),
        [(WALL0 + 950, Src::Engine, Conf::Claim)],
        "NEGATIVE CONTROL: an edge after the newest row read is the claim's"
    );
    r.edge(Claimed::Rested, 990, true);
    r.ordered();

    let mut lost = Rig::bound();
    lost.now = 100;
    lost.tx(prompt("v1"));
    let t1 = lost.turn();
    lost.tx(TxEvent::Overflow { n: 1 });
    lost.now = 900;
    lost.tx(END);
    assert_eq!(
        lost.tr.turn(),
        Some(t1),
        "PRECONDITION: the desync lost the end"
    );
    lost.tr.tx_stopped();
    lost.now = 1_000;
    let closed = ops(&lost.edge(Claimed::Rested, 895, true), Op::Close, Lvl::Turn);
    assert_eq!(
        closed
            .iter()
            .map(|c| (c.node, c.t, c.src))
            .collect::<Vec<_>>(),
        [(t1, WALL0 + 895, Src::Engine)],
        "NEGATIVE CONTROL: a lost row is not read"
    );
    lost.ordered();
}

/// A TURN A PARTIAL ROLLOUT OPENED AND CLOSED HOLDS ITS SPAN: the tap read the vendor turn's prompt
/// and its end before the host fed the engine's edges of it, so the turn the prompt opened (fix
/// E1) is the span's and the edges write nothing; the sampled `working` read after it writes
/// nothing either (it opened a second turn for the span before). NEGATIVE CONTROL: a turn the
/// prompt opened more than [`ATTRIBUTION_MS`] before the beginning is another span's, so the
/// beginning opens its own.
#[test]
pub(crate) fn a_turn_a_partial_rollout_opened_and_closed_holds_its_span() {
    let rollout = || {
        let mut r = Rig::bound_at(TxState::Partial);
        r.now = 100;
        let own = ops(&r.tx(prompt("v1")), Op::Open, Lvl::Turn);
        assert_eq!(own[0].src, Src::Transcript);
        r.now = 120;
        let closed = ops(&r.tx(END), Op::Close, Lvl::Turn);
        assert_eq!(closed[0].node, own[0].node, "a partial end closes its own");
        r
    };
    let mut r = rollout();
    r.now = 1_000;
    for (c, ms) in [(Claimed::Working, 105), (Claimed::Rested, 118)] {
        let recs = r.edge(c, ms, true);
        assert!(recs.is_empty(), "{c:?}: {recs:?}");
    }
    assert_eq!(turn_opens(&r).len(), 1, "one turn for the span");
    r.ordered();

    let mut sampled = rollout();
    sampled.now = 130;
    assert!(sampled.claim(Claimed::Working).is_empty());
    sampled.now = 140;
    assert!(sampled.claim(Claimed::Rested).is_empty());
    assert_eq!(turn_opens(&sampled).len(), 1, "the sampled twin too");
    sampled.ordered();

    let mut stale = rollout();
    stale.now = 7_000;
    let begin = 100 + ATTRIBUTION_MS + 1_000;
    let opened = ops(
        &stale.edge(Claimed::Working, begin, true),
        Op::Open,
        Lvl::Turn,
    );
    assert_eq!(
        opened
            .iter()
            .map(|o| (o.t, o.src, o.conf))
            .collect::<Vec<_>>(),
        [(WALL0 + begin, Src::Engine, Conf::Claim)],
        "NEGATIVE CONTROL: a stale rollout turn holds no later span"
    );
    stale.edge(Claimed::Rested, begin + 17, true);
    assert_eq!(turn_opens(&stale).len(), 2);
    stale.ordered();
}

/// A TURN BEGUN DURING A SEAMLESS UPDATE IS STILL A CLAIMED TURN: the predecessor's partial
/// rollout read a quick turn no beginning took, and the next vendor turn began before the
/// successor adopted, so its prompt row is below the successor's floor. The successor's first
/// `working` opens that turn, as no untaken turn is carried (ATERM-KI-1029-1 R5): a carried one
/// would be taken by that beginning, which then opens none, and the new turn would be in no log.
#[test]
pub(crate) fn a_turn_begun_during_a_seamless_update_is_still_a_claimed_turn() {
    let mut r = Rig::bound_at(TxState::Partial);
    r.now = 100;
    r.tx(prompt("v1"));
    r.now = 120;
    r.tx(END);
    r.now = 125;
    let line = r.tr.carry_line(at(r.now));
    r.now = 200;
    r.adopt(&line);
    assert!(
        r.tx_at(prompt("v2"), WALL0 + 150).is_empty(),
        "the new turn's prompt is below the floor"
    );
    r.now = 210;
    let opened = ops(&r.claim(Claimed::Working), Op::Open, Lvl::Turn);
    assert_eq!(
        opened
            .iter()
            .map(|o| (o.t, o.src, o.conf))
            .collect::<Vec<_>>(),
        [(WALL0 + 210, Src::Engine, Conf::Claim)],
        "the successor's first working opens the new turn"
    );
    r.now = 300;
    r.claim(Claimed::Rested);
    assert_eq!(turn_opens(&r).len(), 2);
    r.ordered();
}

/// AN OPEN PARTIAL TURN IS THE SPAN'S AND CLOSES AT THE ENGINE'S END: the prompt opened the turn
/// before the host fed the beginning, which takes it as the turn running; the engine's end closes
/// it there (`src=engine`), and the vendor turn's end row, read late, only gives it its numbers.
#[test]
pub(crate) fn an_open_partial_turn_is_the_spans_and_closes_at_the_engines_end() {
    let mut r = Rig::bound_at(TxState::Partial);
    r.now = 100;
    r.tx(prompt("v1"));
    let t1 = r.turn();
    r.now = 1_000;
    assert!(r.edge(Claimed::Working, 105, true).is_empty());
    assert_eq!(r.tr.turn(), Some(t1));
    let closed = ops(&r.edge(Claimed::Rested, 300, true), Op::Close, Lvl::Turn);
    assert_eq!(
        closed
            .iter()
            .map(|c| (c.node, c.t, c.out(), c.src, c.conf))
            .collect::<Vec<_>>(),
        [(t1, WALL0 + 300, Some("done"), Src::Engine, Conf::Claim)],
    );
    r.now = 1_200;
    let end = r.tx_at(END, WALL0 + 290);
    assert!(ops(&end, Op::Close, Lvl::Turn).is_empty(), "{end:?}");
    assert_eq!((end[0].node, end[0].f.get_u(Key::VdurMs)), (t1, Some(4200)));
    assert_eq!(turn_opens(&r).len(), 1, "one turn");
    r.ordered();
}

/// TURNS WHOSE EDGES SHARE ONE BATCH CLOCK ARE PAIRED IN ORDER: two vendor turns the partial
/// rollout read (the first opened and closed, the second open), then the engine's edges of both
/// fed with the first three at one batch's time. Each beginning takes the oldest untaken turn, so
/// the run holds two turns and the second closes at its own end. Without a rollout the four edges
/// at one stamp give two claimed turns, the second opened no earlier than the first's close; and
/// a beginning dated before the close of a turn the rollout ended late opens at that close.
#[test]
pub(crate) fn turns_whose_edges_share_one_batch_clock_are_paired_in_order() {
    let mut r = Rig::bound_at(TxState::Partial);
    r.now = 100;
    r.tx(prompt("v1"));
    r.now = 110;
    r.tx(END);
    r.now = 120;
    r.tx(prompt("v2"));
    let t2 = r.turn();
    r.now = 1_000;
    for c in [Claimed::Working, Claimed::Rested, Claimed::Working] {
        let recs = r.edge(c, 130, true);
        assert!(recs.is_empty(), "{c:?}: {recs:?}");
    }
    let closed = ops(&r.edge(Claimed::Rested, 150, true), Op::Close, Lvl::Turn);
    assert_eq!(
        closed
            .iter()
            .map(|c| (c.node, c.t, c.src))
            .collect::<Vec<_>>(),
        [(t2, WALL0 + 150, Src::Engine)],
        "the second span's turn closes at the second end"
    );
    assert_eq!(turn_opens(&r).len(), 2, "two spans, two turns");
    r.ordered();

    let mut bare = Rig::new();
    bare.now = 1_000;
    for c in [
        Claimed::Working,
        Claimed::Rested,
        Claimed::Working,
        Claimed::Rested,
    ] {
        bare.edge(c, 130, true);
    }
    let opens = turn_opens(&bare);
    let closes = ops(&bare.records(), Op::Close, Lvl::Turn);
    assert!(
        opens.len() == 2
            && opens
                .iter()
                .all(|o| (o.src, o.conf) == (Src::Engine, Conf::Claim))
            && closes.len() == 2
            && opens[1].t >= closes[0].t,
        "{:?}",
        bare.records()
    );
    bare.ordered();

    let mut late = Rig::bound_at(TxState::Partial);
    late.now = 100;
    late.tx(prompt("v1"));
    late.edge(Claimed::Working, 105, true);
    late.now = 400;
    late.tx(END);
    late.now = 1_000;
    assert!(late.edge(Claimed::Rested, 390, true).is_empty());
    let next = ops(&late.edge(Claimed::Working, 390, true), Op::Open, Lvl::Turn);
    assert_eq!(
        next.iter().map(|o| (o.t, o.src)).collect::<Vec<_>>(),
        [(WALL0 + 400, Src::Engine)],
        "a sibling turn never opens before the one before it closed"
    );
    late.edge(Claimed::Rested, 450, true);
    late.ordered();
}

/// A REST LEAVES A LATER VENDOR TURN OPEN: the tap read the next vendor turn's prompt (its turn
/// open, fix E1) before the parse reached the engine's end of the turn before it. That end
/// writes nothing, the next beginning takes the open turn as its span's, and the next end closes
/// it there. The sampled rest read in that order leaves it open too, and so does the earlier
/// turn's rest once the later turn's Esc was read: dated before the later turn opened, it is not
/// that turn's end, which closes it `out=interrupted`.
#[test]
pub(crate) fn a_rest_leaves_a_later_vendor_turn_open() {
    let rollout = || {
        let mut r = Rig::bound_at(TxState::Partial);
        r.now = 100;
        r.tx(prompt("v1"));
        r.now = 1_000;
        assert!(r.edge(Claimed::Working, 105, true).is_empty());
        r.now = 1_100;
        r.tx_at(END, WALL0 + 120);
        r.tx_at(prompt("v2"), WALL0 + 130);
        r
    };
    let mut r = rollout();
    let t2 = r.turn();
    let rest = r.edge(Claimed::Rested, 125, true);
    assert!(rest.is_empty(), "{rest:?}");
    assert_eq!(r.tr.turn(), Some(t2));
    assert!(r.edge(Claimed::Working, 135, true).is_empty());
    let closed = ops(&r.edge(Claimed::Rested, 300, true), Op::Close, Lvl::Turn);
    assert_eq!(
        closed
            .iter()
            .map(|c| (c.node, c.t, c.src))
            .collect::<Vec<_>>(),
        [(t2, WALL0 + 300, Src::Engine)]
    );
    assert_eq!(turn_opens(&r).len(), 2, "two spans, two turns");
    r.ordered();

    let mut sampled = rollout();
    let t2 = sampled.turn();
    sampled.now = 1_200;
    let rest = sampled.claim(Claimed::Rested);
    assert!(rest.is_empty(), "{rest:?}");
    assert_eq!(
        sampled.tr.turn(),
        Some(t2),
        "the sampled rest leaves it open"
    );
    sampled.ordered();

    let mut esc = rollout();
    let t2 = esc.turn();
    esc.tx_at(TxEvent::Interrupt, WALL0 + 140);
    let rest = esc.edge(Claimed::Rested, 125, true);
    assert!(rest.is_empty(), "the earlier turn's rest: {rest:?}");
    assert!(esc.edge(Claimed::Working, 135, true).is_empty());
    let closed = ops(&esc.edge(Claimed::Rested, 300, true), Op::Close, Lvl::Turn);
    assert_eq!(
        closed
            .iter()
            .map(|c| (c.node, c.t, c.out()))
            .collect::<Vec<_>>(),
        [(t2, WALL0 + 300, Some("interrupted"))]
    );
    assert_eq!(turn_opens(&esc).len(), 2, "two spans, two turns");
    esc.ordered();
}

/// A CLAIMED TURN WHOSE END NEVER CAME CLOSES `out=unknown` AT THE NEXT BEGINNING: two engine
/// beginnings with no end fed between them (the end fell out of the engine's record) close the
/// first claimed turn there and open the second at that time. NEGATIVE CONTROL: a sampled second
/// `working` is the turn running, so it keeps one turn.
#[test]
pub(crate) fn a_claimed_turn_whose_end_never_came_closes_unknown_at_the_next_begin() {
    let mut r = Rig::new();
    r.now = 1_000;
    r.edge(Claimed::Working, 100, true);
    let t1 = r.turn();
    let recs = r.edge(Claimed::Working, 500, true);
    let closed = ops(&recs, Op::Close, Lvl::Turn);
    let opened = ops(&recs, Op::Open, Lvl::Turn);
    assert_eq!(
        closed
            .iter()
            .map(|c| (c.node, c.t, c.out(), c.src, c.conf))
            .collect::<Vec<_>>(),
        [(t1, WALL0 + 500, Some("unknown"), Src::Engine, Conf::Claim)],
        "{recs:?}"
    );
    assert_eq!(
        opened
            .iter()
            .map(|o| (o.t, o.src, o.conf))
            .collect::<Vec<_>>(),
        [(WALL0 + 500, Src::Engine, Conf::Claim)],
        "{recs:?}"
    );
    r.edge(Claimed::Rested, 600, true);
    r.ordered();

    let mut sampled = Rig::new();
    sampled.now = 100;
    sampled.claim(Claimed::Working);
    sampled.now = 500;
    assert!(
        sampled.claim(Claimed::Working).is_empty(),
        "NEGATIVE CONTROL: a sampled working is the turn running"
    );
    assert_eq!(turn_opens(&sampled).len(), 1);
    sampled.ordered();
}

/// A REFUSED BEGINNING OPENS NOTHING AND ITS END CLOSES NOTHING: the run's bucket (`may_open`)
/// refuses, so the turn is not logged. The bucket is asked once for a beginning that opens a turn
/// and never for one that takes the rollout's turn.
#[test]
pub(crate) fn a_refused_begin_opens_nothing_and_its_end_closes_nothing() {
    let mut r = Rig::new();
    r.now = 1_000;
    for (c, ms) in [(Claimed::Working, 100), (Claimed::Rested, 117)] {
        let recs = r.edge(c, ms, false);
        assert!(recs.is_empty(), "{c:?}: {recs:?}");
    }
    assert_eq!(r.tr.turn(), None);
    let mut asked = 0;
    r.tr.claim_edge(&mut r.log, Claimed::Working, at(200), &mut || {
        asked += 1;
        true
    });
    assert!(r.tr.turn().is_some() && asked == 1, "asked {asked}");
    r.edge(Claimed::Rested, 217, true);
    r.ordered();

    let mut taken = Rig::bound_at(TxState::Partial);
    taken.now = 100;
    taken.tx(prompt("v1"));
    let mut asked = 0;
    taken
        .tr
        .claim_edge(&mut taken.log, Claimed::Working, at(105), &mut || {
            asked += 1;
            true
        });
    assert_eq!(
        asked, 0,
        "a beginning that takes the rollout's turn spends nothing"
    );
    taken.ordered();
}

/// A LATE PARTIAL PROMPT NAMES EACH EDGE-DATED TURN: two whole turns the engine's edges dated,
/// then a partial rollout bound and read, its prompts a few ms after each beginning. Each prompt
/// names its own claimed turn (the claim's closed turns a late prompt names are `src=engine
/// conf=claim`) and no transcript turn opens.
#[test]
pub(crate) fn a_late_partial_prompt_names_each_edge_dated_turn() {
    let mut r = Rig::new();
    r.now = 3_000;
    let mut claimed = Vec::new();
    for (from, to) in [(1_000, 1_017), (2_000, 2_030)] {
        r.edge(Claimed::Working, from, true);
        claimed.push(r.turn());
        r.edge(Claimed::Rested, to, true);
    }
    r.now = 5_000;
    r.state = TxState::Partial;
    r.bind(0);
    for ((vref, from, to), node) in [("v1", 1_002, 1_015), ("v2", 2_002, 2_028)]
        .into_iter()
        .zip(claimed)
    {
        let named = r.tx_at(prompt(vref), WALL0 + from);
        assert!(ops(&named, Op::Open, Lvl::Turn).is_empty(), "{named:?}");
        let amend = named
            .iter()
            .find(|x| x.op == Op::Amend)
            .expect("the prompt names a turn");
        assert_eq!((amend.node, vref_of(amend)), (node, Some(vref)));
        let end = r.tx_at(END, WALL0 + to);
        assert_eq!(
            (end[0].node, end[0].f.get_u(Key::VdurMs)),
            (node, Some(4200))
        );
    }
    assert_eq!(turn_opens(&r).len(), 2, "two spans, two turns");
    r.ordered();
}

/// A CARRIED CLAIMED TURN CLOSES AT AN END-ONLY EDGE: a successor adopts a tracker whose claimed
/// turn runs (the engine's record of it began before the successor, so the host feeds its end
/// alone), and that end closes the turn at its own time.
#[test]
pub(crate) fn a_carried_claimed_turn_closes_at_an_end_only_edge() {
    let mut r = Rig::new();
    r.now = 100;
    r.claim(Claimed::Working);
    let t1 = r.turn();
    r.now = 200;
    let line = r.tr.carry_line(at(r.now));
    r.adopt(&line);
    assert_eq!(r.tr.turn(), Some(t1));
    r.now = 1_000;
    let closed = ops(&r.edge(Claimed::Rested, 250, true), Op::Close, Lvl::Turn);
    assert_eq!(
        closed
            .iter()
            .map(|c| (c.node, c.t, c.out(), c.src))
            .collect::<Vec<_>>(),
        [(t1, WALL0 + 250, Some("done"), Src::Engine)]
    );
    r.ordered();
}

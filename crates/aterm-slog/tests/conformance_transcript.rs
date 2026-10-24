// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests for [`TurnTracker::transcript`] against `transcript_join_model`.

use std::collections::{BTreeMap, BTreeSet};

use aterm_models_session::transcript_join_model;
use aterm_slog::{
    ATTRIBUTION_MS, BoxWord, Conf, Cursor, Key, Kind, Log, Lvl, Node, Op, Open, Rec, SETTLE_MS,
    ScreenStep, SettleCause, ShortId, Src, Stamp, TurnTracker, TxEvent, TxPos, TxState, Verdict,
    WallJudge, WallKind,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, admits, with_buggy};
use aterm_spec::rng::XorShift;

use crate::conformance_turn::path_to;
use crate::support::{WALL0, agent_session, at};

const PROCESS_START: u64 = WALL0 - 60_000;

/// A broken join, for the negative controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Broken {
    /// The shipping join.
    No,
    /// A replayed row is handed on at a fresh offset, past the watermark.
    IgnoresThrough,
    /// A tool call the join dropped is opened under the run.
    ToolUnderRun,
    /// A prompt that came after its turn's window is dated back to the turn.
    AttachEarly,
    /// A prompt row dated in the past is handed on dated now.
    PromptAtNow,
    /// A turn end dated before the open turn opened is handed on dated now.
    EndAtNow,
}

/// The real tracker and its log, the transcript file the join reads, and the model ghost.
pub struct Join {
    pub log: Log,
    pub run: Node,
    pub tracker: TurnTracker,
    /// Test ms (the stamp is [`at`] of it).
    pub now: u64,
    fp: u64,
    off: u64,
    partial: bool,
    last: Option<(TxEvent<'static>, TxPos)>,
    ids: u64,
    /// The floor (wall ms) the join must hold: the run's open, then each
    /// adoption.
    floor: u64,
    /// Every turn's open time.
    opened: BTreeMap<Node, u64>,
    /// Every transcript step's call id, and whether its result was sent.
    tools: Vec<(ShortId, Node, bool)>,
    /// The model's ghost: the first wrong record a step appended.
    pub g: i64,
    broken: Broken,
}

impl Join {
    pub fn new(broken: Broken) -> Join {
        let mut log = Log::new(0);
        let run = agent_session(&mut log, 0);
        Join {
            log,
            run,
            tracker: TurnTracker::new(run),
            now: 0,
            fp: 0x7a11_0000,
            off: 0,
            partial: false,
            last: None,
            ids: 0,
            floor: WALL0,
            opened: BTreeMap::new(),
            tools: Vec::new(),
            g: 0,
            broken,
        }
    }

    fn stamp(&self) -> Stamp {
        at(self.now)
    }

    pub fn wall(&self) -> u64 {
        WALL0 + self.now
    }

    const fn state(&self) -> TxState {
        if self.partial {
            TxState::Partial
        } else {
            TxState::On
        }
    }

    fn id(&mut self, prefix: &str) -> ShortId {
        self.ids += 1;
        ShortId::new(&format!("{prefix}{}", self.ids)).expect("an id")
    }

    const fn pos(&self, off: u64, wall_ms: u64) -> TxPos {
        TxPos {
            fp: self.fp,
            id: Some((1, self.fp)),
            off,
            wall_ms,
        }
    }

    fn ghost(&mut self, g: i64) {
        if self.g == 0 {
            self.g = g;
        }
    }

    /// The records appended after seq `after`.
    pub fn since(&self, after: u64) -> Vec<Rec> {
        let read = self.log.read(Cursor::new(self.log.ep(), after), usize::MAX);
        assert!(read.gap.is_none(), "a step appended past the ring");
        read.recs.into_iter().cloned().collect()
    }

    pub fn observe(&mut self, v: Verdict) {
        let stamp = self.stamp();
        self.tracker.observe(&mut self.log, v, None, stamp);
    }

    fn tick(&mut self) {
        let stamp = self.stamp();
        self.tracker.tick(&mut self.log, stamp);
    }

    /// Runs one `transcript` call at `pos` and returns the appended records.
    pub fn call(&mut self, ev: TxEvent<'static>, pos: TxPos) -> Vec<Rec> {
        let seq0 = self.log.last_seq();
        let held = self.tracker.view(self.now).step;
        let (state, stamp) = (self.state(), self.stamp());
        self.tracker
            .transcript(&mut self.log, ev, pos, state, stamp);
        let recs = self.since(seq0);
        if let Some((n, what, _)) = held
            && !self.log.is_open(n)
            && !recs
                .iter()
                .any(|r| r.op == Op::Close && r.node == n && r.out() == Some("retracted"))
        {
            let wall_held = self
                .tracker
                .view(self.now)
                .step
                .is_some_and(|(_, w, _)| matches!(w, ScreenStep::Wall(_)));
            match what {
                ScreenStep::Wall(_) if wall_held => {}
                ScreenStep::Wall(_) => self.ghost(7),
                _ => self.ghost(6),
            }
        }
        if state == TxState::Partial
            && recs.iter().any(|r| {
                r.src == Src::Transcript
                    && match r.op {
                        Op::Close => {
                            !matches!(r.k, Kind::Tool | Kind::Subagent | Kind::Compact)
                                || r.out() == Some("retracted")
                        }
                        Op::Amend => {
                            r.f.get(Key::Out).is_some() || r.f.get(Key::Confirmed).is_some()
                        }
                        _ => false,
                    }
            })
        {
            self.ghost(8);
        }
        recs
    }

    /// One row at the next offset, dated `wall_ms`.
    pub fn row(&mut self, ev: TxEvent<'static>, wall_ms: u64) -> Vec<Rec> {
        self.off += 160;
        let pos = self.pos(self.off, wall_ms);
        if !matches!(ev, TxEvent::Wall { .. }) {
            self.last = Some((ev, pos));
        }
        self.call(ev, pos)
    }

    fn read(&mut self, recs: &[Rec]) {
        for r in recs {
            if r.op == Op::Open && r.node.lvl == Lvl::Turn {
                self.opened.insert(r.node, r.t);
            }
            if r.op == Op::Open
                && matches!(r.k, Kind::Tool | Kind::Subagent)
                && let Some(aterm_slog::Val::Id(id)) = r.f.get(Key::Vref)
            {
                self.tools.push((*id, r.node, false));
            }
            if r.src == Src::Screen
                && (r.f.get(Key::Vref).is_some() || r.f.get(Key::Confirmed).is_some())
            {
                self.ghost(2);
            }
        }
    }

    /// Adopts the tracker's carry line, with or without its `tx_` tokens.
    pub fn adopt(&mut self, keep_tx: bool) {
        let stamp = self.stamp();
        let line = self.tracker.carry_line(stamp);
        let line = if keep_tx {
            line
        } else {
            line.split(' ')
                .filter(|t| !t.starts_with("tx_"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        self.tracker = TurnTracker::from_carry_line(&line, stamp).expect("its own line reads");
        self.floor = self.floor.max(self.wall());
    }

    fn open_turn(&self) -> Option<(Node, u64)> {
        open_turn(&self.log, self.run)
    }
}

fn open_turn(log: &Log, run: Node) -> Option<(Node, u64)> {
    log.open_under(run, Lvl::Turn).next().map(|e| (e.node, e.t))
}

fn open_tools(log: &Log, run: Node) -> Vec<Node> {
    let mut parents = vec![run];
    parents.extend(log.open_under(run, Lvl::Turn).map(|e| e.node));
    parents
        .into_iter()
        .flat_map(|p| {
            log.open_under(p, Lvl::Step)
                .filter(|e| matches!(e.kind, Kind::Tool | Kind::Subagent))
                .map(|e| e.node)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The projection onto the model's variables.
pub fn project(j: &Join) -> State {
    project_parts(&j.log, j.run, &j.tracker, j.now, j.partial, j.g)
}

/// Projects the log, agent `run`, and tracker state onto the model's variables.
pub fn project_parts(
    log: &Log,
    run: Node,
    tracker: &TurnTracker,
    now_ms: u64,
    partial: bool,
    g: i64,
) -> State {
    let tv = tracker.tx_view();
    let view = tracker.view(now_ms);
    let now = WALL0 + now_ms;
    let st = log.state();
    let turns = log.open_under(run, Lvl::Turn).count();
    let (lt, old) = open_turn(log, run).map_or((0, 0), |(n, t)| {
        let vref = st
            .turn()
            .filter(|o| o.node == n)
            .is_some_and(|o| o.f.get(Key::Vref).is_some());
        (
            if vref { 2 } else { 1 },
            i64::from(t + ATTRIBUTION_MS < now),
        )
    });
    let lc = st.last[1].as_ref().is_some_and(|c| {
        c.f.get(Key::Vref).is_none()
            && c.opened_t
                .is_some_and(|o| o >= WALL0 && o.abs_diff(now) <= ATTRIBUTION_MS)
    });
    let lv = st.last[1]
        .as_ref()
        .is_some_and(|c| c.f.get(Key::Vref).is_none() && c.opened_t.is_some_and(|o| o >= WALL0));
    let vm = match (tv.vref, tv.mapped) {
        (None, _) => 0,
        (Some(_), None) => 3,
        (Some(_), Some(n)) if log.is_open(n) => 1,
        (Some(_), Some(_)) => 2,
    };
    let (ss, wr) = match view.step {
        Some((_, ScreenStep::Wall(_), true)) => (2, 0),
        Some((_, ScreenStep::Wall(_), false)) => (0, 1),
        Some((_, _, true)) => (1, 0),
        Some((_, _, false)) | None => (0, 0),
    };
    let pend = view
        .settle
        .is_some_and(|(c, _)| matches!(c, SettleCause::Wall(_)));
    let vc = open_turn(log, run).is_some_and(|(n, t)| {
        tv.mapped == Some(n)
            && tv.prompt_ms <= t
            && st
                .turn()
                .filter(|o| o.node == n)
                .is_some_and(|o| o.f.get(Key::Vref).is_none() && o.f.get(Key::Cont).is_some())
    });
    [
        ("wm", i64::from(tv.through.is_some())),
        ("pt", i64::from(partial)),
        ("lt", lt),
        ("old", old),
        ("lc", i64::from(lc)),
        ("lv", i64::from(lv)),
        ("vm", vm),
        ("ve", i64::from(tv.ended)),
        (
            "vo",
            i64::from(vm == 3 && tv.prompt_ms.saturating_add(ATTRIBUTION_MS) < now),
        ),
        ("ss", ss),
        ("wr", wr),
        ("pend", i64::from(pend)),
        ("wd", i64::from(view.word == Verdict::Idle)),
        ("rf", i64::from(tv.refuted.is_some())),
        ("ts", open_tools(log, run).len() as i64),
        ("vc", i64::from(vc)),
        ("g", if turns > 1 { 1 } else { g }),
    ]
    .into_iter()
    .collect()
}

fn admitted(
    m: &Model,
    j: &Join,
    actions: &[&str],
    prev: &State,
    next: &State,
    seq0: u64,
) -> Result<(), String> {
    let states = path_to(m, actions, prev, next).ok_or_else(|| {
        let broken: Vec<&str> = m
            .invariants
            .iter()
            .filter(|i| !m.check_invariant(i.name, next))
            .map(|i| i.name)
            .collect();
        format!(
            "real step is not the model's {actions:?} (it breaks {broken:?}):\n  from {prev:?}\n  to   {next:?}"
        )
    })?;
    for s in &states {
        if let Some(inv) = m.invariants.iter().find(|i| !m.check_invariant(i.name, s)) {
            return Err(format!("{} broken along real {actions:?}: {s:?}", inv.name));
        }
    }
    let (mut ups, mut downs) = (0, 0);
    for (w, a) in states.windows(2).zip(actions) {
        if *a == "PromptSupersede" {
            ups += 1;
            downs += 1;
        } else if w[0]["lt"] == 0 && w[1]["lt"] > 0 {
            ups += 1;
        } else if w[0]["lt"] > 0 && w[1]["lt"] == 0 {
            downs += 1;
        }
    }
    let recs = j.since(seq0);
    let turn_recs = |op: Op| {
        recs.iter()
            .filter(|r| r.op == op && r.node.lvl == Lvl::Turn)
            .count()
    };
    if (turn_recs(Op::Open), turn_recs(Op::Close)) != (ups, downs) {
        return Err(format!(
            "turn records of real {actions:?} are not the model's: {recs:?}"
        ));
    }
    Ok(())
}

/// One step of a walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Busy,
    Box,
    Wall,
    Rest,
    WallRest,
    Age,
    Bind,
    Lapse,
    Adopt,
    Carry,
    Flip,
    Prompt,
    /// A prompt dated older than the open turn by more than the window.
    PromptOld,
    /// A prompt dated at the newest closed turn's open while a turn is open.
    PromptNear,
    Tool,
    Result,
    End,
    /// A turn end dated just before the open turn opened.
    EarlyEnd,
    Interrupt,
    Refute,
    Confirm,
    Compact,
    Replay,
    OldRow,
}

const PROMPTS: [&str; 6] = [
    "PromptAttach",
    "PromptShare",
    "PromptSupersede",
    "PromptLate",
    "PromptUnmapped",
    "PromptOpens",
];

/// Applies one step on `j` and checks it against the model.
pub fn apply(m: &Model, j: &mut Join, step: Step) -> Result<Option<Vec<&'static str>>, String> {
    let prev = project(j);
    let seq0 = j.log.last_seq();
    let on = |a: &str| m.action_enabled(a, &prev);
    let word = j.tracker.view(j.now).word;
    let wall = j.wall();
    let path: Vec<&'static str> = match step {
        Step::Busy => {
            if word == Verdict::Busy {
                return Ok(None);
            }
            j.observe(Verdict::Busy);
            vec!["ScreenBusy"]
        }
        Step::Box => {
            if !on("ScreenBox") {
                return Ok(None);
            }
            j.observe(Verdict::Prompt(BoxWord::from_detail("bash:not-read-only")));
            vec!["ScreenBox"]
        }
        Step::Wall => {
            if !on("ScreenWall") {
                return Ok(None);
            }
            let k = if word == Verdict::Wall(WallKind::UsageSession) {
                WallKind::Overloaded
            } else {
                WallKind::UsageSession
            };
            j.observe(Verdict::Wall(k));
            vec!["ScreenWall"]
        }
        Step::Rest => {
            if !on("ScreenRest") {
                return Ok(None);
            }
            if word != Verdict::Idle {
                j.observe(Verdict::Idle);
            }
            j.now += SETTLE_MS;
            j.tick();
            vec!["ScreenRest"]
        }
        Step::WallRest => {
            if !on("WallRest") {
                return Ok(None);
            }
            j.now += SETTLE_MS;
            j.tick();
            vec!["WallRest"]
        }
        Step::Age => {
            if !on("Age") {
                return Ok(None);
            }
            j.now += ATTRIBUTION_MS + 1;
            vec!["Age"]
        }
        Step::Bind => {
            j.fp += 1;
            j.off = 4096;
            let bind = TxEvent::Bind {
                len: j.off,
                floor_ms: PROCESS_START,
            };
            let pos = j.pos(j.off, wall);
            j.call(bind, pos);
            vec!["Bind"]
        }
        Step::Lapse => {
            if !on("Lapse") {
                return Ok(None);
            }
            let bind = TxEvent::Bind {
                len: j.off,
                floor_ms: PROCESS_START,
            };
            let pos = j.pos(j.off, wall);
            j.call(bind, pos);
            if let Some((ev, pos)) = j.last {
                let seq = j.log.last_seq();
                j.call(ev, pos);
                if seq != j.log.last_seq() {
                    j.ghost(3);
                }
            }
            vec!["Lapse"]
        }
        Step::Adopt => {
            j.adopt(false);
            vec!["AdoptWithoutWatermark"]
        }
        Step::Carry => {
            j.adopt(true);
            vec!["Carry"]
        }
        Step::Flip => {
            let a = if j.partial { "SetOn" } else { "SetPartial" };
            j.partial = !j.partial;
            vec![a]
        }
        Step::Prompt => {
            let path: Vec<&'static str> = PROMPTS.into_iter().filter(|a| on(a)).collect();
            let vref = j.id("p");
            let tp = wall;
            let dated = match (j.broken, j.open_turn()) {
                (Broken::AttachEarly, Some((_, t))) if t + ATTRIBUTION_MS < tp => t,
                _ => tp,
            };
            let recs = j.row(TxEvent::Prompt { vref }, dated);
            if recs.iter().any(|r| {
                r.op == Op::Amend
                    && r.f.get(Key::Vref).is_some()
                    && j.opened
                        .get(&r.node)
                        .is_some_and(|o| o.abs_diff(tp) > ATTRIBUTION_MS)
            }) {
                j.ghost(5);
            }
            path
        }
        Step::PromptOld | Step::PromptNear => {
            let Some((_, opened)) = j.open_turn() else {
                return Ok(None);
            };
            let closed = j.log.state().last[1].as_ref().and_then(|c| {
                c.opened_t
                    .map(|o| (c.node, o, c.f.get(Key::Vref).is_some()))
            });
            let tp = match (step, closed) {
                (Step::PromptOld, _) => opened.checked_sub(ATTRIBUTION_MS + 1),
                (_, Some((_, o, false))) if o != opened => Some(o),
                _ => None,
            };
            let Some(tp) = tp.filter(|&t| t >= j.floor) else {
                return Ok(None);
            };
            let near = closed.filter(|&(_, o, held)| {
                !held
                    && o >= WALL0
                    && o.abs_diff(tp) <= ATTRIBUTION_MS
                    && o.abs_diff(tp) < opened.abs_diff(tp)
            });
            let path: Vec<&'static str> = match near {
                Some(_) => ["PromptLateNear"].into_iter().filter(|a| on(a)).collect(),
                None => ["PromptOld"].into_iter().filter(|a| on(a)).collect(),
            };
            let vref = j.id("q");
            let dated = if j.broken == Broken::PromptAtNow {
                wall
            } else {
                tp
            };
            let recs = j.row(TxEvent::Prompt { vref }, dated);
            let named: Vec<Node> = recs
                .iter()
                .filter(|r| r.op == Op::Amend && r.f.get(Key::Vref).is_some())
                .map(|r| r.node)
                .collect();
            if named.iter().any(|n| {
                j.opened
                    .get(n)
                    .is_some_and(|o| o.abs_diff(tp) > ATTRIBUTION_MS)
            }) {
                j.ghost(5);
            }
            if let Some((node, ..)) = near
                && prev["wm"] == 1
                && named != [node]
            {
                j.ghost(9);
            }
            path
        }
        Step::Tool => {
            if prev["ts"] >= 2 {
                return Ok(None);
            }
            let id = j.id("t");
            let subagent = j.ids.is_multiple_of(3);
            let ev = TxEvent::ToolUse {
                id,
                tool: if subagent { "Task" } else { "Bash" },
                cls: if subagent { "agent" } else { "shell" },
                subagent,
            };
            let recs = j.row(ev, wall);
            if j.broken == Broken::ToolUnderRun
                && prev["wm"] == 1
                && !recs.iter().any(|r| r.op == Op::Open)
            {
                let mut f = aterm_slog::Fields::new();
                let _ = f.set(Key::Vref, aterm_slog::Val::Id(id));
                let open = Open::new(Kind::Tool, Src::Transcript, Conf::Exact)
                    .under(j.run)
                    .fields(f);
                let stamp = j.stamp();
                let _ = j.log.open(open, stamp);
            }
            if on("ToolUse") {
                vec!["ToolUse"]
            } else {
                vec![]
            }
        }
        Step::Result => {
            let open = j
                .tools
                .iter()
                .position(|(_, n, sent)| !sent && j.log.is_open(*n));
            let owed = j.tools.iter().position(|(_, _, sent)| !sent);
            let Some(i) = open.or(owed) else {
                return Ok(None);
            };
            j.tools[i].2 = true;
            let ev = TxEvent::ToolResult {
                id: j.tools[i].0,
                error: j.ids % 2 == 1,
            };
            j.row(ev, wall);
            if open.is_some() && on("ToolResult") {
                vec!["ToolResult"]
            } else {
                vec![]
            }
        }
        Step::End => {
            let end = TxEvent::TurnEnd {
                vdur_ms: 4200,
                tok_in: 900,
                tok_out: 120,
            };
            j.row(end, wall);
            ["TxEndCloses", "TxEndHeld"]
                .into_iter()
                .filter(|a| on(a))
                .collect()
        }
        Step::EarlyEnd => {
            let Some((node, opened)) = j.open_turn() else {
                return Ok(None);
            };
            let Some(dated) = opened.checked_sub(1).filter(|&t| t >= j.floor) else {
                return Ok(None);
            };
            let end = TxEvent::TurnEnd {
                vdur_ms: 4200,
                tok_in: 900,
                tok_out: 120,
            };
            let dated = if j.broken == Broken::EndAtNow {
                wall
            } else {
                dated
            };
            let recs = j.row(end, dated);
            if prev["vc"] == 1 && recs.iter().any(|r| r.op == Op::Close && r.node == node) {
                j.ghost(10);
            }
            if on("TxEndBeforeCont") {
                vec!["TxEndBeforeCont"]
            } else {
                ["TxEndCloses", "TxEndHeld"]
                    .into_iter()
                    .filter(|a| on(a))
                    .collect()
            }
        }
        Step::Interrupt => {
            j.row(TxEvent::Interrupt, wall);
            if on("Interrupt") {
                vec!["Interrupt"]
            } else {
                vec![]
            }
        }
        Step::Refute | Step::Confirm => {
            let kind = match j.tracker.view(j.now).step {
                Some((_, ScreenStep::Wall(k), _)) => k,
                _ => WallKind::UsageSession,
            };
            let (judge, a) = if step == Step::Refute {
                (WallJudge::Refute, "Refute")
            } else {
                (WallJudge::Confirm, "Confirm")
            };
            j.row(TxEvent::Wall { kind, judge }, wall);
            if on(a) { vec![a] } else { vec![] }
        }
        Step::Compact => {
            j.row(TxEvent::Compact, wall);
            if on("Compact") {
                vec!["Compact"]
            } else {
                vec![]
            }
        }
        Step::Replay => {
            let Some((ev, pos)) = j.last else {
                return Ok(None);
            };
            let pos = if j.broken == Broken::IgnoresThrough {
                j.off += 1;
                TxPos { off: j.off, ..pos }
            } else {
                pos
            };
            if !j.call(ev, pos).is_empty() {
                j.ghost(3);
            }
            if on("Replay") { vec!["Replay"] } else { vec![] }
        }
        Step::OldRow => {
            let vref = j.id("o");
            let dated = j.floor - 1000;
            let recs = j.row(TxEvent::Prompt { vref }, dated);
            if recs
                .iter()
                .any(|r| !(r.op == Op::Mark && r.k == Kind::Source))
            {
                j.ghost(4);
            }
            if on("OldRow") { vec!["OldRow"] } else { vec![] }
        }
    };
    let recs = j.since(seq0);
    j.read(&recs);
    admitted(m, j, &path, &prev, &project(j), seq0)?;
    Ok(Some(path))
}

const POOL: &[Step] = &[
    Step::Busy,
    Step::Busy,
    Step::Box,
    Step::Wall,
    Step::Rest,
    Step::Rest,
    Step::WallRest,
    Step::Age,
    Step::Bind,
    Step::Lapse,
    Step::Adopt,
    Step::Carry,
    Step::Flip,
    Step::Prompt,
    Step::Prompt,
    Step::Prompt,
    Step::PromptOld,
    Step::PromptNear,
    Step::Tool,
    Step::Tool,
    Step::Result,
    Step::Result,
    Step::End,
    Step::End,
    Step::EarlyEnd,
    Step::EarlyEnd,
    Step::Interrupt,
    Step::Refute,
    Step::Confirm,
    Step::Compact,
    Step::Replay,
    Step::OldRow,
];

/// Runs one seeded walk of `n` steps and returns the actions it drove.
pub fn walk(seed: u64, n: usize, broken: Broken) -> Result<BTreeSet<&'static str>, String> {
    let m = transcript_join_model();
    let mut rng = XorShift::new(seed);
    let mut j = Join::new(broken);
    let mut ran = BTreeSet::new();
    if rng.below(4) > 0 {
        apply(&m, &mut j, Step::Bind)?;
    }
    for i in 0..n {
        let step = POOL[rng.index(POOL.len())];
        match apply(&m, &mut j, step).map_err(|e| format!("seed {seed} step {i} {step:?}: {e}"))? {
            Some(path) if path.is_empty() => {
                ran.insert("(a row the join ignored)");
            }
            Some(path) => ran.extend(path),
            None => {}
        }
    }
    Ok(ran)
}

#[test]
fn transcript_join_conforms_to_derived_model() {
    let m = transcript_join_model();
    let mut ran = BTreeSet::new();
    for seed in 1..=160u64 {
        match walk(seed, 160, Broken::No) {
            Ok(r) => ran.extend(r),
            Err(e) => panic!("{e}"),
        }
    }
    let dead: BTreeSet<&str> = [
        "PromptOpensBeside",
        "LateStepOrphan",
        "RestLeavesSteps",
        "ScreenConfirms",
        "ReplayAfterLapse",
        "ReplayFromProcess",
        "AttachEarly",
        "OldPromptAttaches",
        "LateNamesOpen",
        "MergeClosesBox",
        "TxCloseTakesWall",
        "RefuteLeavesSettle",
        "PartialCloses",
        "EarlyEndClosesCont",
    ]
    .into_iter()
    .collect();
    for a in &m.actions {
        if !dead.contains(a.name) {
            assert!(ran.contains(a.name), "the walks never drove `{}`", a.name);
        }
    }
    assert!(ran.contains("(a row the join ignored)"));
}

#[test]
fn transcript_join_conformance_rejects_each_broken_join() {
    for (broken, law) in [
        (Broken::IgnoresThrough, "NoReplay"),
        (Broken::ToolUnderRun, "StepsUnderOpenTurn"),
        (Broken::AttachEarly, "AttachOnlyWithinWindow"),
        (Broken::PromptAtNow, "PromptNamesNearestTurn"),
        (Broken::EndAtNow, "EndClosesOnlyItsOwnTurn"),
    ] {
        let caught: Vec<String> = (1..=160u64)
            .filter_map(|seed| walk(seed, 160, broken).err())
            .collect();
        assert!(!caught.is_empty(), "{broken:?} passed every walk");
        assert!(
            caught.iter().any(|e| e.contains(law)),
            "{broken:?} was never caught by {law}: {}",
            caught[0]
        );
    }
}

#[test]
fn transcript_join_conformance_rejects_each_mutant_state() {
    let m = transcript_join_model();
    let buggy = with_buggy(&m, 1);
    let reach = |path: &[&str]| {
        let mut s = m.init_state();
        for a in path {
            assert!(m.fire(a, &mut s), "{a} from {s:?}");
        }
        s
    };
    let cases: [(&str, &[&str], &str); 14] = [
        (
            "PromptOpensBeside",
            &["Bind", "ScreenBusy", "PromptAttach"],
            "AtMostOneOpenTurn",
        ),
        (
            "LateStepOrphan",
            &["Bind", "ScreenBusy", "PromptAttach", "ScreenRest"],
            "StepsUnderOpenTurn",
        ),
        (
            "RestLeavesSteps",
            &["Bind", "ScreenBusy", "PromptAttach", "ToolUse"],
            "StepsUnderOpenTurn",
        ),
        (
            "ScreenConfirms",
            &["ScreenBusy", "ScreenWall"],
            "ConfirmedOnlyFromTranscript",
        ),
        ("ReplayAfterLapse", &["Bind", "Lapse"], "NoReplay"),
        ("ReplayFromProcess", &["Bind"], "NoTurnBeforeRunOpen"),
        (
            "AttachEarly",
            &["Bind", "ScreenBusy", "Age"],
            "AttachOnlyWithinWindow",
        ),
        (
            "OldPromptAttaches",
            &["Bind", "ScreenBusy"],
            "AttachOnlyWithinWindow",
        ),
        (
            "LateNamesOpen",
            &["Bind", "ScreenBusy", "ScreenRest", "ScreenBusy"],
            "PromptNamesNearestTurn",
        ),
        (
            "MergeClosesBox",
            &["Bind", "ScreenBusy", "PromptAttach", "ScreenBox"],
            "TxNeverClosesScreenStep",
        ),
        (
            "TxCloseTakesWall",
            &["Bind", "ScreenBusy", "PromptAttach", "ScreenWall"],
            "WallNeedOutlivesTxClose",
        ),
        (
            "RefuteLeavesSettle",
            &["Bind", "ScreenBusy", "ScreenWall"],
            "RefutedWallStaysShut",
        ),
        (
            "PartialCloses",
            &["Bind", "SetPartial", "ScreenBusy", "PromptAttach"],
            "PartialNeverOverrules",
        ),
        (
            "EarlyEndClosesCont",
            &[
                "Bind",
                "ScreenBusy",
                "PromptAttach",
                "ScreenRest",
                "ScreenBusy",
            ],
            "EndClosesOnlyItsOwnTurn",
        ),
    ];
    for (mutant, path, law) in cases {
        let from = reach(path);
        let broken = buggy.successors(mutant, &from);
        assert!(!broken.is_empty(), "{mutant} is not enabled after {path:?}");
        for b in broken {
            assert!(
                admits(&m, &from, &b).is_none(),
                "{mutant}'s step is the model's"
            );
            assert!(!m.check_invariant(law, &b), "{mutant} keeps {law}");
        }
    }
}

#[test]
fn transcript_join_projection_sees_an_orphan_step() {
    let m = transcript_join_model();
    let mut j = Join::new(Broken::No);
    for step in [Step::Bind, Step::Busy, Step::Prompt, Step::Tool] {
        apply(&m, &mut j, step).expect("the shipping join");
    }
    let mut s = project(&j);
    assert_eq!((s["lt"], s["vm"], s["ts"]), (2, 1, 1));
    s.insert("lt", 0);
    assert!(!m.check_invariant("StepsUnderOpenTurn", &s));
}

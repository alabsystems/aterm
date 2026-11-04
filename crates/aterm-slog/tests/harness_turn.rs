// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A HARNESS TURN (gap 25): a vendor row that says the agent's own harness began its turn
//! amends that turn `turn_by=harness`, read `state=on` only, once, only where the open names
//! nobody, and only on the turn its prompt mapped; the act the host paired with it comes on its
//! own amend, once, and only on such a turn; the brief prints both.

use aterm_slog::{
    Actor, Conf, DEFAULT_BUDGET, Key, Log, Lvl, Node, Op, Rec, ShortId, Src, TurnTracker, TxEvent,
    TxPos, TxState, Val,
};

use crate::support::{WALL0, agent_session, at, lines, time_order_holds};

const FP: u64 = 0xc1a0_de00_0000_0025;
const ID: Option<(u64, u64)> = Some((16_777_220, 2_525));

struct Rig {
    log: Log,
    tr: TurnTracker,
    /// Test ms.
    now: u64,
    off: u64,
    state: TxState,
}

impl Rig {
    fn bound_at(state: TxState) -> Rig {
        let mut log = Log::new(1);
        let run = agent_session(&mut log, 0);
        let mut r = Rig {
            log,
            tr: TurnTracker::new(run),
            now: 0,
            off: 0,
            state,
        };
        let pos = r.pos(0);
        r.call(
            TxEvent::Bind {
                len: 0,
                floor_ms: WALL0 - 60_000,
            },
            pos,
        );
        r
    }

    const fn pos(&self, off: u64) -> TxPos {
        TxPos {
            fp: FP,
            id: ID,
            off,
            wall_ms: WALL0 + self.now,
        }
    }

    fn call(&mut self, ev: TxEvent<'_>, pos: TxPos) -> Vec<Rec> {
        let seq0 = self.log.last_seq();
        self.tr
            .transcript(&mut self.log, ev, pos, self.state, at(self.now));
        self.log
            .window()
            .filter(|r| r.seq > seq0)
            .cloned()
            .collect()
    }

    fn tx(&mut self, ev: TxEvent<'_>) -> Vec<Rec> {
        self.off += 200;
        let pos = self.pos(self.off);
        self.call(ev, pos)
    }

    /// The prompt `v` at test ms `ms`: the turn it opened.
    fn prompt(&mut self, v: &str, ms: u64) -> Node {
        self.now = ms;
        let recs = self.tx(TxEvent::Prompt { vref: id(v) });
        let opened: Vec<&Rec> = recs
            .iter()
            .filter(|r| r.op == Op::Open && r.node.lvl == Lvl::Turn)
            .collect();
        assert_eq!(opened.len(), 1, "{recs:?}");
        opened[0].node
    }

    fn harness(&mut self, v: &str, ms: u64) -> Vec<Rec> {
        self.now = ms;
        self.tx(TxEvent::HarnessTurn { vref: id(v) })
    }

    /// The host pairs the act mark `seq` with `turn` at test ms `ms`: whether the tracker wrote,
    /// and what.
    fn cause(&mut self, turn: Node, seq: u64, ms: u64) -> (bool, Vec<Rec>) {
        self.now = ms;
        let seq0 = self.log.last_seq();
        let wrote = self.tr.harness_cause(&mut self.log, turn, seq, at(ms));
        let recs = self
            .log
            .window()
            .filter(|r| r.seq > seq0)
            .cloned()
            .collect();
        (wrote, recs)
    }

    fn amends(&self, node: Node) -> Vec<String> {
        self.keyed(node, Key::TurnBy)
    }

    /// The amends of `node` carrying `key`, encoded.
    fn keyed(&self, node: Node, key: Key) -> Vec<String> {
        self.log
            .window()
            .filter(|r| r.node == node && r.op == Op::Amend && r.f.get(key).is_some())
            .map(|r| aterm_slog::encode(r, aterm_slog::Tier::Content).expect("fits"))
            .collect()
    }

    /// The brief's line for turn `n`.
    fn brief_turn(&self, n: u32) -> String {
        let b = self
            .log
            .brief("s-1", DEFAULT_BUDGET, WALL0 + self.now + 1_000);
        let head = format!("t{n} ");
        b.text
            .lines()
            .find(|l| l.trim_start().starts_with(&head))
            .unwrap_or_else(|| panic!("no t{n} line:\n{}", b.text))
            .to_string()
    }

    fn ordered(&self) {
        let got = lines(&self.log);
        time_order_holds(&got).unwrap_or_else(|e| panic!("{e}: {got:#?}"));
    }
}

fn id(s: &str) -> ShortId {
    ShortId::new(s).expect("a short id")
}

#[test]
fn a_harness_turn_names_its_turn_by_harness_and_its_act() {
    let mut r = Rig::bound_at(TxState::On);
    let t1 = r.prompt("v1", 500);
    let open = r
        .log
        .window()
        .find(|x| x.node == t1 && x.op == Op::Open)
        .cloned()
        .expect("t1's open");
    assert_eq!(
        (open.by, open.src, open.conf),
        (Actor::Nobody, Src::Transcript, Conf::Exact)
    );
    let recs = r.harness("v1", 600);
    assert_eq!(recs.len(), 1, "{recs:?}");
    let a = &recs[0];
    assert_eq!(
        (a.op, a.node, a.by, a.src, a.conf),
        (Op::Amend, t1, Actor::Vendor, Src::Transcript, Conf::Exact)
    );
    assert_eq!(a.f.get_word(Key::TurnBy), Some("harness"));
    assert_eq!(a.f.get(Key::Cause), None, "the act comes on its own amend");
    let said = r.amends(t1);
    assert_eq!(said.len(), 1);
    assert!(
        said[0].contains(" amend t1 turn turn_by=harness by=vendor src=transcript conf=exact"),
        "{said:?}"
    );
    let (wrote, recs) = r.cause(t1, 17, 700);
    assert!(wrote);
    assert_eq!(recs.len(), 1, "{recs:?}");
    let c = &recs[0];
    assert_eq!(
        (c.op, c.node, c.by, c.src, c.conf),
        (Op::Amend, t1, Actor::Vendor, Src::Transcript, Conf::Exact)
    );
    assert_eq!(c.f.get(Key::Cause), Some(&Val::Id(id("17"))));
    assert_eq!(c.f.get(Key::TurnBy), None, "the role was said once");
    let caused = r.keyed(t1, Key::Cause);
    assert!(
        caused.len() == 1
            && caused[0].contains(
                " amend t1 turn cause=17 cause_k=rec by=vendor src=transcript conf=exact"
            ),
        "{caused:?}"
    );
    let reopened = r.log.window().find(|x| x.node == t1 && x.op == Op::Open);
    assert_eq!(
        reopened.map(|x| x.by),
        Some(Actor::Nobody),
        "the open's by= stands"
    );
    let line = r.brief_turn(1);
    assert!(line.contains(" by=harness cause=17"), "{line}");
    r.ordered();
    let mut bare = Rig::bound_at(TxState::On);
    let t = bare.prompt("v1", 500);
    let _ = bare.harness("v1", 600);
    let said = bare.amends(t);
    assert_eq!(said.len(), 1);
    assert!(
        said[0].contains(" turn_by=harness ") && !said[0].contains(" cause="),
        "{said:?}"
    );
    let line = bare.brief_turn(1);
    assert!(
        line.contains(" by=harness") && !line.contains(" cause="),
        "{line}"
    );
}

#[test]
fn a_harness_turn_never_overrules_an_observed_input() {
    let mut r = Rig::bound_at(TxState::On);
    r.now = 400;
    r.tr.note_input(Actor::Human, at(400));
    let t1 = r.prompt("v1", 1_400);
    let open = r.log.window().find(|x| x.node == t1 && x.op == Op::Open);
    assert_eq!(open.map(|x| x.by), Some(Actor::Human));
    assert!(r.harness("v1", 1_500).is_empty());
    assert!(r.amends(t1).is_empty());
    let line = r.brief_turn(1);
    assert!(
        line.contains(" by=human") && !line.contains("harness"),
        "{line}"
    );
    r.ordered();
}

#[test]
fn a_harness_turn_at_partial_or_again_writes_nothing() {
    let mut r = Rig::bound_at(TxState::Partial);
    let t1 = r.prompt("v1", 500);
    assert!(r.harness("v1", 600).is_empty());
    assert!(
        r.amends(t1).is_empty(),
        "a source read partial names no starter"
    );
    let mut r = Rig::bound_at(TxState::On);
    let t1 = r.prompt("v1", 500);
    assert_eq!(r.harness("v1", 600).len(), 1);
    assert!(r.harness("v1", 700).is_empty(), "once");
    assert_eq!(r.amends(t1).len(), 1);
    r.ordered();
}

#[test]
fn a_harness_turn_names_only_the_turn_its_prompt_mapped() {
    let mut r = Rig::bound_at(TxState::On);
    let t1 = r.prompt("v1", 500);
    assert!(r.harness("v2", 600).is_empty());
    assert!(r.amends(t1).is_empty());
    assert_eq!(
        r.harness("v1", 700).len(),
        1,
        "NEGATIVE CONTROL: its own prompt's turn"
    );
}

/// A HARNESS TURN'S CAUSE IS ONE AMEND: the host's act reaches only a turn amended
/// `turn_by=harness` (not a turn its row never named, nor one whose open names an observed
/// input), once whatever the host pairs later, and still after the turn ended, since the pair can
/// settle at a read past its end.
#[test]
fn a_harness_turns_cause_is_amended_once_and_only_on_a_harness_turn() {
    let mut r = Rig::bound_at(TxState::On);
    let t1 = r.prompt("v1", 500);
    let (wrote, recs) = r.cause(t1, 17, 600);
    assert!(!wrote && recs.is_empty(), "no harness turn: {recs:?}");
    let _ = r.harness("v1", 700);
    r.now = 900;
    let _ = r.tx(TxEvent::TurnEnd {
        vdur_ms: 400,
        tok_in: 0,
        tok_out: 0,
    });
    assert!(
        r.log.window().any(|x| x.node == t1 && x.op == Op::Close),
        "the turn ended before its pair settled"
    );
    let (wrote, recs) = r.cause(t1, 17, 3_000);
    assert!(wrote && recs.len() == 1, "{recs:?}");
    let (wrote, recs) = r.cause(t1, 18, 3_100);
    assert!(!wrote && recs.is_empty(), "once: {recs:?}");
    assert_eq!(r.keyed(t1, Key::Cause).len(), 1);
    let line = r.brief_turn(1);
    assert!(line.contains(" by=harness cause=17"), "{line}");
    r.ordered();
    let mut seen = Rig::bound_at(TxState::On);
    seen.now = 400;
    seen.tr.note_input(Actor::Human, at(400));
    let t = seen.prompt("v1", 1_400);
    let _ = seen.harness("v1", 1_500);
    let (wrote, recs) = seen.cause(t, 17, 1_600);
    assert!(
        !wrote && recs.is_empty(),
        "an observed input stands: {recs:?}"
    );
}

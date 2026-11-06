// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1: the real herald's told-once memory refines `HeraldToldOnce` (design §14.18).

use super::{
    Escalation, EscalationKind, Herald, NOTIFY_BURST_WINDOW, NeedId, NeedRead, NeedWord,
    StatusCarry, headline,
};
use aterm_models_gui::herald_told_once_model;
use aterm_spec::interp::State;
use std::collections::BTreeSet;
use std::time::Instant;

/// The two needs' ids, and the entry key each set is shown under ({a}, {b}, {a, b}), a box's
/// key (another kind in front), all apart.
const A: u64 = 11;
const B: u64 = 12;
const KEYS: [u64; 3] = [101, 102, 103];
const BOX: u64 = 104;

pub(super) struct Rig {
    herald: Herald,
    now: Instant,
    /// The owner's last plain Enter, a count each process starts at 0.
    turn: u64,
    /// The escalation noted last, which the owner's turn notes again.
    last: Option<Escalation>,
    /// Notices naming each need since the last forget, capped at 2.
    named: (i64, i64),
    /// The needs the last step brought into view.
    new: (i64, i64),
    fresh: i64,
}

fn ask(key: u64, needs: &[u64], turn: u64) -> Escalation {
    let body = |id: u64| if id == A { "a" } else { "b" }.to_string();
    Escalation {
        session: 0,
        kind: EscalationKind::Attention,
        word: NeedWord::Ask,
        key,
        label: format!("ask {key}"),
        body: body(needs[0]),
        headline: headline(NeedWord::Ask),
        report: None,
        needs: needs
            .iter()
            .map(|&id| NeedId {
                id,
                read: NeedRead::Whole,
                body: body(id),
            })
            .collect(),
        owner_turn: turn,
    }
}

fn boxed(turn: u64) -> Escalation {
    Escalation {
        session: 0,
        kind: EscalationKind::Prompt,
        word: NeedWord::Approval,
        key: BOX,
        label: "box".to_string(),
        body: "box".to_string(),
        headline: headline(NeedWord::Approval),
        report: None,
        needs: Vec::new(),
        owner_turn: turn,
    }
}

impl Rig {
    fn new(told_off: bool) -> Self {
        Self {
            herald: Herald {
                told_off,
                ..Herald::default()
            },
            now: Instant::now(),
            turn: 0,
            last: None,
            named: (0, 0),
            new: (0, 0),
            fresh: 0,
        }
    }

    /// The model's view of the real herald: what its slot shows (the needs it noted last, under
    /// an ask's key; 0 for none or the box), the key's set, the told memory as it stands at the
    /// owner's turn now (`Herald::told_ids`: what its next note judges by and a restart carries),
    /// whether that memory is carried and met no need yet, and the rig's own counts of what the
    /// notices named.
    pub(super) fn project(&self) -> State {
        let slot = self.herald.slots.get(&0);
        let ask_shown = slot
            .and_then(|s| s.shown)
            .and_then(|(kind, key)| (kind == EscalationKind::Attention).then_some(key));
        let noted = |id: u64| slot.is_some_and(|s| s.told.noted.contains(&id));
        let shown = ask_shown.map_or(0, |_| i64::from(noted(A)) + 2 * i64::from(noted(B)));
        let key = match slot.and_then(|s| s.shown) {
            None => 0,
            Some((EscalationKind::Attention, key)) => KEYS
                .iter()
                .position(|k| *k == key)
                .map_or(-1, |i| i as i64 + 1),
            Some(_) => 4,
        };
        let memory = self.herald.told_ids(0, self.turn);
        let told = |id: u64| i64::from(memory.contains(&id));
        State::from([
            ("shown", shown),
            ("key", key),
            ("told_a", told(A)),
            ("told_b", told(B)),
            ("named_a", self.named.0),
            ("named_b", self.named.1),
            ("new_a", self.new.0),
            ("new_b", self.new.1),
            ("fresh", self.fresh),
            ("carried", i64::from(slot.is_some_and(|s| s.told.carried))),
        ])
    }

    fn note(&mut self, current: Option<Escalation>) {
        let out = self.herald.note(0, current.as_ref(), false, true, self.now);
        match out.notice.map(|n| n.body).as_deref() {
            Some("a") => self.named.0 = (self.named.0 + 1).min(2),
            Some("b") => self.named.1 = (self.named.1 + 1).min(2),
            _ => {}
        }
        self.last = current;
    }

    pub(super) fn step(&mut self, action: &str) {
        self.now += NOTIFY_BURST_WINDOW;
        let before = self.project();
        let (was_a, was_b) = (before["shown"] & 1, before["shown"] >> 1);
        self.fresh = 0;
        match action {
            "ShowA" => self.note(Some(ask(KEYS[0], &[A], self.turn))),
            "ShowB" => self.note(Some(ask(KEYS[1], &[B], self.turn))),
            "ShowBoth" => self.note(Some(ask(KEYS[2], &[A, B], self.turn))),
            "Swap" => {
                let key = KEYS[usize::try_from(before["key"] - 1).unwrap()];
                self.note(Some(ask(key, &[A, B], self.turn)));
            }
            "Displace" => self.note(Some(boxed(self.turn))),
            "Clear" => {
                // With a carried memory no need has met yet, nothing standing is the restart's
                // gap, not a take-down: it forgets nothing.
                let gap = before["carried"] == 1;
                self.note(None);
                if !gap {
                    self.named = (0, 0);
                    self.fresh = 2;
                }
            }
            "Restart" => {
                // A cold restore: the leaf carries the told ids alone, as the outgoing process
                // exports them at the owner's turn there, and a new process, whose turns count
                // from 0 again, adopts them and notes its first publication, before the agent
                // raises anything again: the restart's gap.
                let carry = StatusCarry {
                    told_ids: self.herald.told_ids(0, self.turn),
                    ..StatusCarry::default()
                };
                self.herald = Herald {
                    told_off: self.herald.told_off,
                    ..Herald::default()
                };
                self.turn = 0;
                self.herald.adopt(0, carry, 0, self.now);
                self.note(None);
            }
            "OwnerTurn" => {
                self.turn += 1;
                let again = self.last.take().map(|mut esc| {
                    esc.owner_turn = self.turn;
                    esc
                });
                self.note(again);
                self.named = (0, 0);
                self.fresh = 1;
            }
            _ => unreachable!("{action}"),
        }
        let after = self.project();
        let (is_a, is_b) = (after["shown"] & 1, after["shown"] >> 1);
        self.new = (is_a & (1 - was_a), is_b & (1 - was_b));
    }
}

const ACTIONS: [&str; 8] = [
    "ShowA",
    "ShowB",
    "ShowBoth",
    "Swap",
    "Displace",
    "Clear",
    "OwnerTurn",
    "Restart",
];

#[test]
fn herald_told_once_conforms_to_real_herald() {
    let model = herald_told_once_model();
    let mut visited = BTreeSet::new();
    for mut schedule in 0..ACTIONS.len().pow(6) {
        let mut rig = Rig::new(false);
        let mut state = model.init_state();
        assert_eq!(rig.project(), state);
        let mut walked = Vec::new();
        for _ in 0..6 {
            let action = ACTIONS[schedule % ACTIONS.len()];
            schedule /= ACTIONS.len();
            if !model.action_enabled(action, &state) {
                break;
            }
            walked.push(action);
            rig.step(action);
            assert!(model.fire(action, &mut state));
            assert_eq!(rig.project(), state, "after {walked:?}");
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, &state),
                    "{} after {walked:?}",
                    invariant.name
                );
            }
            visited.insert(action);
        }
    }
    assert_eq!(visited, BTreeSet::from(ACTIONS));

    // Negative control: the herald before told once posts a need shown, displaced and shown
    // again twice, which the model's ToldOnce refuses.
    let mut before = Rig::new(true);
    for action in ["ShowA", "Displace", "ShowA"] {
        before.step(action);
    }
    assert_eq!(
        before.named,
        (2, 0),
        "the herald before told once tells it twice"
    );
    assert!(!model.check_invariant("ToldOnce", &before.project()));
    let mut told = Rig::new(false);
    for action in ["ShowA", "Displace", "ShowA"] {
        told.step(action);
    }
    assert_eq!(told.named, (1, 0));
    assert!(model.check_invariant("ToldOnce", &told.project()));
    // Negative control: a restart that carries nothing tells a need told before it again.
    let mut forgets = Rig::new(false);
    forgets.step("ShowA");
    forgets.now += NOTIFY_BURST_WINDOW;
    forgets.herald = Herald::default();
    forgets.turn = 0;
    forgets.note(None);
    forgets.step("ShowA");
    assert_eq!(
        forgets.named,
        (2, 0),
        "a restart with no carry tells it twice"
    );
    assert!(!model.check_invariant("ToldOnce", &forgets.project()));
    // Negative control: a restart's gap read as a take-down (the carried mark off) forgets the
    // memory, and the need told before it is told again.
    let mut gap = Rig::new(false);
    gap.step("ShowA");
    gap.step("Restart");
    gap.herald
        .slots
        .get_mut(&0)
        .expect("the restart's carried slot")
        .told
        .carried = false;
    gap.now += NOTIFY_BURST_WINDOW;
    gap.note(None);
    gap.step("ShowA");
    assert_eq!(gap.named, (2, 0), "a gap that forgets tells it twice");
    assert!(!model.check_invariant("ToldOnce", &gap.project()));
    let mut kept = Rig::new(false);
    for action in ["ShowA", "Restart", "Clear", "ShowA"] {
        kept.step(action);
    }
    assert_eq!(kept.named, (1, 0), "the carried memory rides the gap");
}

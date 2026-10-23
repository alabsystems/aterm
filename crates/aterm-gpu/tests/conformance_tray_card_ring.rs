// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests binding `TrayCardRing` to `MetalTrayCardRing`.

use std::collections::BTreeMap;

use aterm_gpu::tray_card_ring::{TrayCardRing, present_may_read};
use aterm_models_render::metal_tray_card_ring_model;
use aterm_spec::derive::Model;

/// The model's `MaxSeq`: how many presents a drive may commit.
const MAX_SEQ: u64 = 4;
/// The card's height in rows; a change is one row band of it.
const HEIGHT: u32 = 8;
/// How many steps a drive takes: every sequence up to it is driven.
const DEPTH: usize = 7;

type State = BTreeMap<&'static str, i64>;

/// Predicate selecting presents that a rewrite of `last`'s slot awaits.
type Rule = fn(u64, u64) -> bool;

/// The rule before 969c8abb08.
fn old_rule(seq: u64, last: u64) -> bool {
    seq >= last
}

/// No rewrite waits for anything.
fn no_settle(_seq: u64, _last: u64) -> bool {
    false
}

/// The reader a readback's tray bake records.
type BakeReader = fn(u64) -> Option<u64>;

fn bake_records_none(_next: u64) -> Option<u64> {
    None
}

fn bake_records_next(next: u64) -> Option<u64> {
    Some(next)
}

/// The rules a drive runs.
#[derive(Clone, Copy)]
struct Rules {
    settle: Rule,
    bake: BakeReader,
    /// Whether a refused present after Submit B retains its sequence number.
    refused_keeps: bool,
}

/// The renderer's rules.
const RULES: Rules = Rules {
    settle: present_may_read,
    bake: bake_records_none,
    refused_keeps: true,
};

/// One step of a drive.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// This window presents the card unchanged.
    Draw,
    /// The card changed and this window presents it.
    Change,
    /// A readback's tray bake met a changed card (no present).
    Bake,
    /// Another window presents: no slot of this ring is sampled.
    Bare,
    /// The GPU finishes the oldest in-flight present.
    Complete,
    /// The card changed size: a fresh ring, and this window presents it.
    Remint,
    /// This window presents the card unchanged.
    Refused,
}

const STEPS: [Step; 7] = [
    Step::Draw,
    Step::Change,
    Step::Bake,
    Step::Bare,
    Step::Complete,
    Step::Remint,
    Step::Refused,
];

/// The real ring beside a simulated queue that knows the truth.
#[derive(Clone)]
struct Sim {
    ring: Option<TrayCardRing>,
    /// The ring's generation: a re-mint's slots are new textures.
    generation: u32,
    /// Presents committed: numbered `0..seq` (`MetalArmLive::present_seq`).
    seq: u64,
    /// Presents complete, in queue order: `0..done`.
    done: u64,
    /// What each present really sampled: `(generation, slot)`, or nothing.
    sampled: Vec<Option<(u32, usize)>>,
    /// Alternates the changed band so stale rows accumulate.
    band_flip: bool,
}

impl Sim {
    fn new() -> Self {
        Self {
            ring: None,
            generation: 0,
            seq: 0,
            done: 0,
            sampled: Vec::new(),
            band_flip: false,
        }
    }

    /// Model index of the last present that sampled `slot` (`0` if none).
    fn real(&self, slot: usize) -> i64 {
        self.sampled
            .iter()
            .rposition(|s| *s == Some((self.generation, slot)))
            .map_or(0, |at| at as i64 + 1)
    }

    /// The model state, `writing` and `before` naming a rewrite under way.
    fn project(&self, writing: i64, before: i64) -> State {
        let last = |slot: usize| {
            self.ring
                .as_ref()
                .and_then(|ring| ring.slot(slot))
                .and_then(|slot| slot.last_read)
                .map_or(0, |reader| reader as i64 + 1)
        };
        BTreeMap::from([
            ("seq", self.seq as i64),
            ("done", self.done as i64),
            ("shown", self.ring.as_ref().map_or(0, |r| r.shown() as i64)),
            ("last0", last(0)),
            ("last1", last(1)),
            ("real0", self.real(0)),
            ("real1", self.real(1)),
            ("writing", writing),
            ("before", before),
        ])
    }

    /// Commit a present that samples the shown slot (or none: `card` false).
    fn present(&mut self, card: bool) {
        let sampled = card
            .then(|| self.ring.as_ref().map(|r| (self.generation, r.shown())))
            .flatten();
        self.sampled.push(sampled);
        self.seq += 1;
    }

    fn band(&mut self) -> (u32, u32) {
        self.band_flip = !self.band_flip;
        if self.band_flip { (1, 3) } else { (5, 7) }
    }

    /// Run `step`, returning each transition it makes as `(before, after, action)`.
    fn step(&mut self, step: Step, rules: Rules) -> Vec<(State, State, String)> {
        let start = self.project(0, 0);
        match step {
            Step::Draw => {
                let Some(ring) = self.ring.as_mut() else {
                    return Vec::new();
                };
                if self.seq >= MAX_SEQ {
                    return Vec::new();
                }
                ring.read_unchanged(Some(self.seq));
                self.present(true);
                vec![(start, self.project(0, 0), "Draw".into())]
            }
            Step::Refused => {
                let Some(ring) = self.ring.as_mut() else {
                    return Vec::new();
                };
                if self.seq >= MAX_SEQ {
                    return Vec::new();
                }
                ring.read_unchanged(Some(self.seq));
                if !rules.refused_keeps {
                    self.done = self.seq;
                    return vec![(start, self.project(0, 0), "Refuse".into())];
                }
                self.present(true);
                let mut transitions = vec![(start, self.project(0, 0), "Draw".into())];
                while self.done < self.seq {
                    let before = self.project(0, 0);
                    self.done += 1;
                    transitions.push((before, self.project(0, 0), "Complete".into()));
                }
                transitions
            }
            Step::Bare => {
                if self.seq >= MAX_SEQ {
                    return Vec::new();
                }
                self.present(false);
                vec![(start, self.project(0, 0), "Bare".into())]
            }
            Step::Complete => {
                if self.done >= self.seq {
                    return Vec::new();
                }
                self.done += 1;
                vec![(start, self.project(0, 0), "Complete".into())]
            }
            Step::Remint => {
                if self.seq >= MAX_SEQ {
                    return Vec::new();
                }
                self.ring = Some(TrayCardRing::fresh(HEIGHT, Some(self.seq)));
                self.generation += 1;
                self.present(true);
                vec![(start, self.project(0, 0), "Remint".into())]
            }
            Step::Change | Step::Bake => {
                let shows = matches!(step, Step::Change);
                if self.ring.is_none() || self.seq >= MAX_SEQ {
                    return Vec::new();
                }
                let band = self.band();
                let write = self.ring.as_ref().expect("checked above").plan_change(band);
                let target = write.slot;
                let before = self.done;
                if let Some(last) = write.settle_through {
                    for seq in self.done..self.seq {
                        if (rules.settle)(seq, last) {
                            self.done = self.done.max(seq + 1);
                        }
                    }
                }
                let settled = self.project(if target == 1 { 1 } else { 2 }, before as i64);
                let ring = self.ring.as_mut().expect("checked above");
                let reader = if shows {
                    Some(self.seq)
                } else {
                    (rules.bake)(self.seq)
                };
                ring.land_change(write, band, reader);
                if shows {
                    self.present(true);
                }
                let suffix = if target == 1 { "1" } else { "0" };
                vec![
                    (start, settled.clone(), format!("Settle{suffix}")),
                    (
                        settled,
                        self.project(0, 0),
                        format!("{}{suffix}", if shows { "Show" } else { "Bake" }),
                    ),
                ]
            }
        }
    }
}

/// Every transition every sequence of steps up to [`DEPTH`] makes, deduplicated.
fn drive(rules: Rules) -> BTreeMap<(State, State), String> {
    let mut seen = BTreeMap::new();
    let mut frontier = vec![Sim::new()];
    for _ in 0..DEPTH {
        let mut next = Vec::new();
        for sim in &frontier {
            for step in STEPS {
                let mut sim = sim.clone();
                let transitions = sim.step(step, rules);
                if transitions.is_empty() {
                    continue;
                }
                for (before, after, action) in transitions {
                    seen.insert((before, after), action);
                }
                next.push(sim);
            }
        }
        frontier = next;
    }
    seen
}

/// The transitions `model`'s named action does not make.
fn refused(model: &Model, seen: &BTreeMap<(State, State), String>) -> Vec<String> {
    seen.iter()
        .filter(|((before, after), action)| !model.successors(action, before).contains(after))
        .map(|((before, after), action)| format!("{action}: {before:?} -> {after:?}"))
        .collect()
}

fn with_buggy(model: &Model, buggy: i64) -> Model {
    aterm_spec::interp::with_buggy(model, buggy)
}

#[test]
fn the_real_tray_card_ring_conforms_to_the_metal_tray_card_ring_model() {
    let model = metal_tray_card_ring_model();
    let seen = drive(RULES);
    for action in [
        "Draw", "Settle0", "Settle1", "Show0", "Show1", "Bake0", "Bake1", "Bare", "Complete",
        "Remint",
    ] {
        assert!(
            seen.values().any(|seen| seen == action),
            "the drive reaches {action}"
        );
    }
    assert_eq!(refused(&model, &seen), Vec::<String>::new());
}

/// NEGATIVE CONTROLS: the rule before 969c8abb08, no settle, a bake that
/// records the next present, and a refused present that hands its number back
/// each make a transition the committed model refuses and its mutant admits.
#[test]
fn the_old_rule_no_settle_a_reading_bake_and_a_reused_number_are_refused() {
    let model = metal_tray_card_ring_model();
    for (name, rules, buggy) in [
        (
            "the old rule",
            Rules {
                settle: old_rule,
                ..RULES
            },
            1,
        ),
        (
            "no settle",
            Rules {
                settle: no_settle,
                ..RULES
            },
            2,
        ),
        (
            "a bake recording the next present",
            Rules {
                bake: bake_records_next,
                ..RULES
            },
            3,
        ),
        (
            "a refused present handing its number back",
            Rules {
                refused_keeps: false,
                ..RULES
            },
            4,
        ),
    ] {
        let seen = drive(rules);
        assert!(
            !refused(&model, &seen).is_empty(),
            "{name}: the committed model refuses a transition"
        );
        assert_eq!(
            refused(&with_buggy(&model, buggy), &seen),
            Vec::<String>::new(),
            "{name}: the Buggy = {buggy} mutant admits every transition"
        );
        let broken = model
            .invariants
            .iter()
            .filter(|law| {
                seen.keys()
                    .any(|(_, after)| !model.check_invariant(law.name, after))
            })
            .map(|law| law.name)
            .collect::<Vec<_>>();
        assert!(!broken.is_empty(), "{name}: the drive breaks a law");
    }
}

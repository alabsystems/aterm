// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1 OF `FabricExactlyOnce` (`aterm-models-session`, exactly once across the
//! bridge's round at one inbox offset): the simulated world, projected onto the model
//! after every step, one inbox OFFSET at a time.
//!
//! A slot opens when a message of the world's (a marker, [`super::world`]) is first on
//! the bus at an offset of a live session's lane: that is the model's `Publish`. From
//! then on, after every step of the world, the slot's projection is read off what a
//! person could read — the bus, the endpoint's ring and dedup, the inbox group's
//! cursor — and off the bridge's one owed commit:
//!
//! | model | the world |
//! |---|---|
//! | `occ` | 1: the message is at the offset; 2: a later message of the same session is; 0: nothing is |
//! | `lost` | a restart of the log took the message |
//! | `g` | how many times the log's generation moved since the slot opened |
//! | `c > O` | the inbox group resumes past the offset (its commit through it is on the log) |
//! | `pend` | a live bridge owes the group a commit through the offset, ON THE LOG THE BROKER SERVES NOW, and the group is not past it (a commit owed on a log a move replaced names that log's records, and the attach clamps it before it is made) |
//! | `alive` | a bridge runs |
//! | `told` | the endpoint holds the session's offsets on the log the broker serves now |
//! | `dd` | the endpoint remembers the offset (its `off=` dedup) |
//! | `nr`, `nr2` | the session's rows of the message, and of the later one |
//!
//! The step is the model's when some run of at most [`DEPTH`] of the model's actions
//! from the states the slot can be in reaches a state of that projection (`c` is read
//! as "past the offset or not", the world's only distinction). A projection the model
//! cannot reach is a VIOLATION: the round did something the model does not allow — a
//! row twice, a cursor past a record no row holds, or a step the model has no name for.
//! A slot leaves the model's domain, and is no longer followed, when the offset comes
//! to hold a THIRD message or something that is not the session's mail, when the log
//! has moved more than the model's bound, or when its session is gone.
//!
//! `mv`, the kind of move the endpoint is still to be told of, is the model's own
//! bookkeeping: it is read off the reachable states, never projected.

use std::collections::{BTreeMap, BTreeSet};

use aterm_spec::derive::Model;
use aterm_spec::interp::State;

/// The most model actions one step of the world may take, from a bridge pass that
/// tells the endpoint, delivers and commits in one go to a broker restart.
const DEPTH: usize = 8;

/// What one session looks like to the tap.
#[derive(Clone, Debug, Default)]
pub(crate) struct SessionView {
    pub(crate) dedup: BTreeSet<u64>,
    /// `(offset, text)` of every row.
    pub(crate) rows: Vec<(u64, String)>,
    /// The log identity its offsets are on, as `link log` named it.
    pub(crate) log: Option<String>,
}

/// What the tap reads of the world after one step.
#[derive(Clone, Debug, Default)]
pub(crate) struct View {
    /// Every record of the log, by offset: `Some((session, text))` for a message on a
    /// session's lane of this node, `None` for anything else.
    pub(crate) records: Vec<Option<(String, String)>>,
    /// The log's identity, as `link log` names it.
    pub(crate) log: String,
    pub(crate) generation: u64,
    /// Where the inbox group resumes.
    pub(crate) group_start: u64,
    pub(crate) alive: bool,
    /// The commit the running bridge owes on the log the broker serves now, if it
    /// does: one owed on a log a move replaced is the attach's to clamp, not the
    /// offset's ([`super::world`]'s `view`).
    pub(crate) owed: Option<u64>,
    /// The live sessions.
    pub(crate) sessions: BTreeMap<String, SessionView>,
}

/// One offset followed through the model.
#[derive(Debug)]
struct Slot {
    off: u64,
    sid: String,
    first: String,
    later: Option<String>,
    gen0: u64,
    lost: bool,
    /// The model states this slot can be in.
    states: Vec<State>,
    /// The last projection checked.
    last: Option<Key>,
    done: bool,
}

/// A projection: `(occ, lost, g, past, pend, alive, told, dd, nr, nr2)`.
type Key = [i64; 10];

/// The model state's projection.
fn key_of(s: &State, offset: i64) -> Key {
    [
        s["occ"],
        s["lost"],
        s["g"],
        i64::from(s["c"] > offset),
        s["pend"],
        s["alive"],
        s["told"],
        s["dd"],
        s["nr"],
        s["nr2"],
    ]
}

/// THE TAP: every offset the world's messages took, followed through the model.
pub(crate) struct Tap {
    model: Model,
    offset: i64,
    max_g: u64,
    slots: BTreeMap<String, Slot>,
    pub(crate) violations: Vec<String>,
    /// Steps of a slot the model was asked to take (not counting stutters).
    pub(crate) checked: u64,
    /// Slots opened.
    pub(crate) opened: u64,
    /// Which model actions the world's steps were explained by, at least once.
    pub(crate) fired: BTreeSet<&'static str>,
}

impl Tap {
    pub(crate) fn new(model: Model) -> Self {
        let get = |name: &str| {
            model
                .consts
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| *v)
                .expect("the model's constant")
        };
        let offset = get("O");
        let max_g = u64::try_from(get("MaxG")).expect("a bound");
        Self {
            model,
            offset,
            max_g,
            slots: BTreeMap::new(),
            violations: Vec::new(),
            checked: 0,
            opened: 0,
            fired: BTreeSet::new(),
        }
    }

    /// The projection of `slot` in `view`, or `None` once it is out of the model's
    /// domain (the module doc).
    fn project(&self, slot: &mut Slot, view: &View, markers: &BTreeSet<String>) -> Option<Key> {
        let session = view.sessions.get(&slot.sid)?;
        let at = usize::try_from(slot.off).ok()?;
        // A message at another offset than its slot's: taken, and published
        // again (an in-flight entry's replay) — one record, followed nowhere.
        let elsewhere = |text: &String| {
            view.records
                .iter()
                .enumerate()
                .any(|(o, r)| o != at && r.as_ref().is_some_and(|(_, t)| t == text))
        };
        if elsewhere(&slot.first) || slot.later.as_ref().is_some_and(elsewhere) {
            return None;
        }
        let occ = match view.records.get(at) {
            None => {
                if slot.later.is_none() {
                    slot.lost = true;
                }
                if slot.later.is_some() {
                    // The later message was taken too: a third may follow.
                    return None;
                }
                0
            }
            Some(Some((sid, text))) if *sid == slot.sid => {
                if *text == slot.first {
                    if slot.lost {
                        // Taken, and back at its offset: not the model's.
                        return None;
                    }
                    1
                } else if !slot.lost || !markers.contains(text) {
                    // Not taken, or not one of the world's messages (a receipt the
                    // bridge published, whose text is no one message's): out.
                    return None;
                } else {
                    match &slot.later {
                        None => {
                            slot.later = Some(text.clone());
                            2
                        }
                        Some(later) if later == text => 2,
                        Some(_) => return None,
                    }
                }
            }
            Some(_) => return None,
        };
        let g = view.generation.checked_sub(slot.gen0)?;
        if g > self.max_g {
            return None;
        }
        let past = view.group_start > slot.off;
        let pend = view.alive && !past && view.owed.is_some_and(|p| p >= slot.off);
        let rows = |text: Option<&String>| {
            text.map_or(0, |t| {
                session.rows.iter().filter(|(_, r)| r == t).count() as i64
            })
        };
        Some([
            occ,
            i64::from(slot.lost),
            i64::try_from(g).ok()?,
            i64::from(past),
            i64::from(pend),
            i64::from(view.alive),
            i64::from(session.log.as_deref() == Some(view.log.as_str())),
            i64::from(session.dedup.contains(&slot.off)),
            rows(Some(&slot.first)),
            rows(slot.later.as_ref()),
        ])
    }

    /// Every model state reachable from `from` in at most [`DEPTH`] actions whose
    /// projection is `want`, with the actions that reach it.
    fn reach(&self, from: &[State], want: Key) -> Option<(Vec<State>, Vec<&'static str>)> {
        let mut frontier: Vec<(State, Vec<&'static str>)> =
            from.iter().map(|s| (s.clone(), Vec::new())).collect();
        let mut seen: BTreeSet<Vec<i64>> =
            from.iter().map(|s| s.values().copied().collect()).collect();
        for _ in 0..DEPTH {
            let mut next = Vec::new();
            for (state, path) in &frontier {
                for action in self.model.actions.iter().map(|a| a.name) {
                    for succ in self.model.successors(action, state) {
                        if seen.insert(succ.values().copied().collect()) {
                            let mut p = path.clone();
                            p.push(action);
                            next.push((succ, p));
                        }
                    }
                }
            }
            let hits: Vec<(State, Vec<&'static str>)> = next
                .iter()
                .filter(|(s, _)| key_of(s, self.offset) == want)
                .cloned()
                .collect();
            if !hits.is_empty() {
                let path = hits[0].1.clone();
                return Some((hits.into_iter().map(|(s, _)| s).collect(), path));
            }
            if next.is_empty() {
                return None;
            }
            frontier = next;
        }
        None
    }

    /// Follow every slot through one step of the world, opening the slots of the
    /// messages it put on the bus.
    pub(crate) fn observe(&mut self, view: &View, markers: &BTreeSet<String>) {
        let known: BTreeSet<String> = self
            .slots
            .values()
            .flat_map(|s| std::iter::once(s.first.clone()).chain(s.later.clone()))
            .collect();
        for (off, record) in view.records.iter().enumerate() {
            let Some((sid, text)) = record else { continue };
            if !markers.contains(text) || known.contains(text) || !view.sessions.contains_key(sid) {
                continue;
            }
            self.opened += 1;
            self.slots.insert(
                text.clone(),
                Slot {
                    off: off as u64,
                    sid: sid.clone(),
                    first: text.clone(),
                    later: None,
                    gen0: view.generation,
                    lost: false,
                    states: initial(&self.model),
                    last: None,
                    done: false,
                },
            );
        }
        let names: Vec<String> = self.slots.keys().cloned().collect();
        for name in names {
            let Some(mut slot) = self.slots.remove(&name) else {
                continue;
            };
            if !slot.done {
                match self.project(&mut slot, view, markers) {
                    None => slot.done = true,
                    Some(want) if slot.last == Some(want) => {}
                    Some(want) => {
                        self.checked += 1;
                        // A slot opens with the model's `init` as its state, and its first
                        // projection still has the bridge as it is: alive or not.
                        match self.reach(&slot.states, want) {
                            Some((states, path)) => {
                                self.fired.extend(path);
                                slot.states = states;
                                slot.last = Some(want);
                            }
                            None => {
                                self.violations.push(format!(
                                    "model: the round at @{} ({} {}) is not FabricExactlyOnce's: \
                                     {:?} -> {want:?} (occ lost g past pend alive told dd nr nr2)",
                                    slot.off, slot.sid, slot.first, slot.last
                                ));
                                slot.done = true;
                            }
                        }
                    }
                }
            }
            self.slots.insert(name, slot);
        }
    }
}

/// The states a slot opens in: the model's own, and the same with a move the
/// endpoint is still to be told of — a record can land on a moved log before the
/// bridge reattaches. If that move took what sat at the offset before, the
/// endpoint may still remember the offset (`dd`) until the tell forgets it (`mv =
/// 1`); if it took nothing there, it remembers nothing of it (`mv = 2`).
fn initial(model: &Model) -> Vec<State> {
    let init = model.init_state();
    let untold = |mv: i64, dd: i64| {
        let mut s = init.clone();
        s.insert("told", 0);
        s.insert("mv", mv);
        s.insert("dd", dd);
        s
    };
    vec![init.clone(), untold(2, 0), untold(1, 0), untold(1, 1)]
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tap projecting the simulated link world onto `FabricExactlyOnce`.
//!
//! A slot's record retention gave up (below the broker's base and not held: the world's
//! [`View::gone`]) projects as `gone`, the model's `GivenUp`: the group may then commit past
//! it with no row (`OwePast`), and NoSkip holds modulo the give-up. A cut never reads as a
//! loss: the slot's record stays in the log the tap reads, only a power loss takes it.

use std::collections::{BTreeMap, BTreeSet};

use aterm_spec::derive::Model;
use aterm_spec::interp::State;

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
    /// Every record of the log, by offset.
    pub(crate) records: Vec<Option<(String, String)>>,
    /// The offsets of the records retention gave up (sim item B05): below the base and not
    /// held, so no reader is handed them again.
    pub(crate) gone: BTreeSet<u64>,
    /// The log's identity, as `link log` names it.
    pub(crate) log: String,
    pub(crate) generation: u64,
    /// Where the inbox group resumes.
    pub(crate) group_start: u64,
    pub(crate) alive: bool,
    /// The commit the running bridge owes on the log the broker serves now, if it does.
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

type Key = [i64; 11];

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
        s["gone"],
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

    fn project(&self, slot: &mut Slot, view: &View, markers: &BTreeSet<String>) -> Option<Key> {
        let session = view.sessions.get(&slot.sid)?;
        let at = usize::try_from(slot.off).ok()?;
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
                    return None;
                }
                0
            }
            Some(Some((sid, text))) if *sid == slot.sid => {
                if *text == slot.first {
                    if slot.lost {
                        return None;
                    }
                    1
                } else if !slot.lost || !markers.contains(text) {
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
            i64::from(view.gone.contains(&slot.off)),
        ])
    }

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

    /// Follow every slot through one step of the world, opening the slots of the messages it put on
    /// the bus.
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
                        match self.reach(&slot.states, want) {
                            Some((states, path)) => {
                                self.fired.extend(path);
                                slot.states = states;
                                slot.last = Some(want);
                            }
                            None => {
                                self.violations.push(format!(
                                    "model: the round at @{} ({} {}) is not FabricExactlyOnce's: \
                                     {:?} -> {want:?} (occ lost g past pend alive told dd nr nr2 gone)",
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

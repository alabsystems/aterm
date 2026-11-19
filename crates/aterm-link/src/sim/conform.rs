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
#[derive(Clone, Copy, Debug)]
pub(crate) struct SessionView<'a> {
    pub(crate) dedup: &'a BTreeSet<u64>,
    /// Every row.
    pub(crate) rows: &'a [super::endpoint::Row],
    /// The log identity its offsets are on, as `link log` named it.
    pub(crate) log: Option<&'a str>,
}

/// What the tap reads of the world after one step.
#[derive(Clone, Debug)]
pub(crate) struct View<'a> {
    /// Every record of the log, by offset: `(sid, text)` of one on a lane of this node's inbox.
    pub(crate) records: &'a [Option<(String, String)>],
    /// Where each text of `records` stands, as [`add_text`] adds them.
    pub(crate) texts: &'a Texts,
    /// The offsets of the records retention gave up (sim item B05): below the base and not
    /// held, so no reader is handed them again.
    pub(crate) gone: BTreeSet<u64>,
    /// The log's identity, as `link log` names it.
    pub(crate) log: String,
    /// The moves of the log's identity so far (another log's too): the model's `g`.
    pub(crate) generation: u64,
    /// The logs set aside so far (`--retire-log`, ATERM-KI-1111-4): a slot whose record lay on
    /// a log set aside since it opened is no longer followed, since what the model loses is a
    /// power loss's, never a durable record, and the world's own laws judge the rest.
    pub(crate) retired_logs: u64,
    /// The remints aterm forgot across (ATERM-KI-1116-7314): logs astream gave another id
    /// keeping every record, which the bridge could not prove continue the one before, so aterm
    /// forgot what it held. A slot opened before one is no longer followed: the model has no
    /// step for a log that kept its records under an id nobody proved, and the world's own laws
    /// judge the rest, as across a log set aside.
    pub(crate) forgot_remints: u64,
    /// Where the inbox group resumes.
    pub(crate) group_start: u64,
    pub(crate) alive: bool,
    /// The commit the running bridge owes on the log the broker serves now, if it does.
    pub(crate) owed: Option<u64>,
    /// The live sessions.
    pub(crate) sessions: BTreeMap<&'a str, SessionView<'a>>,
}

/// One offset followed through the model.
#[derive(Debug)]
struct Slot {
    off: u64,
    sid: String,
    first: String,
    later: Option<String>,
    gen0: u64,
    /// The logs set aside when it opened ([`View::retired_logs`]).
    retired0: u64,
    /// The remints aterm forgot across when it opened ([`View::forgot_remints`]).
    forgot0: u64,
    lost: bool,
    /// The model states this slot can be in.
    states: Vec<State>,
    /// The last projection checked.
    last: Option<Key>,
    done: bool,
}

type Key = [i64; 11];

/// What [`Tap::reach`] answers: the states the projection reached, and the first one's path.
type Reached = Option<(Vec<State>, Vec<&'static str>)>;

/// Where each text of a [`View`]'s records stands ([`View::texts`]): the first offset holding
/// it, and whether another offset holds it too.
pub(crate) type Texts = BTreeMap<String, (usize, bool)>;

/// [`Texts`] with the record at `off`, `(sid, text)` of one on an inbox lane, added: the records
/// of a [`View`] added in offset order make its [`View::texts`].
pub(crate) fn add_text(texts: &mut Texts, off: usize, record: Option<&(String, String)>) {
    if let Some((_, text)) = record {
        texts
            .entry(text.clone())
            .and_modify(|(_, again)| *again = true)
            .or_insert((off, false));
    }
}

/// Whether a record at an offset other than `at` holds `text`.
pub(crate) fn elsewhere(texts: &Texts, text: &str, at: usize) -> bool {
    texts
        .get(text)
        .is_some_and(|&(first, again)| again || first != at)
}

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
    /// For each of the model's actions in order, the one [`Model::successors`] resolves its
    /// name to.
    actions: Vec<usize>,
    /// [`Tap::reach`]'s answers so far, by the states it started from and the projection it
    /// was asked for: a search is a function of those alone, the model being this tap's.
    reached: BTreeMap<(Vec<Vec<i64>>, Key), Reached>,
    offset: i64,
    max_g: u64,
    slots: BTreeMap<String, Slot>,
    /// Every text a slot followed, first or later: a record holding one opens no slot.
    known: BTreeSet<String>,
    pub(crate) violations: Vec<String>,
    /// Steps of a slot the model was asked to take (not counting stutters).
    pub(crate) checked: u64,
    /// Slots opened.
    pub(crate) opened: u64,
    /// Steps of [`Tap::checked`] of a slot opened after a log was set aside ([`Slot::retired0`]).
    pub(crate) checked_past_a_retire: u64,
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
        let actions = model
            .actions
            .iter()
            .map(|a| {
                model
                    .actions
                    .iter()
                    .position(|b| b.name == a.name)
                    .expect("an action finds itself")
            })
            .collect();
        Self {
            model,
            actions,
            reached: BTreeMap::new(),
            offset,
            max_g,
            slots: BTreeMap::new(),
            known: BTreeSet::new(),
            violations: Vec::new(),
            checked: 0,
            opened: 0,
            checked_past_a_retire: 0,
            fired: BTreeSet::new(),
        }
    }

    fn project(
        &self,
        slot: &mut Slot,
        view: &View<'_>,
        marker: &dyn Fn(&str) -> bool,
    ) -> Option<Key> {
        let session = view.sessions.get(slot.sid.as_str())?;
        if view.retired_logs != slot.retired0 || view.forgot_remints != slot.forgot0 {
            return None;
        }
        let at = usize::try_from(slot.off).ok()?;
        let elsewhere = |text: &String| elsewhere(view.texts, text, at);
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
                } else if !slot.lost || !marker(text) {
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
                session.rows.iter().filter(|r| r.text == *t).count() as i64
            })
        };
        Some([
            occ,
            i64::from(slot.lost),
            i64::try_from(g).ok()?,
            i64::from(past),
            i64::from(pend),
            i64::from(view.alive),
            i64::from(session.log == Some(view.log.as_str())),
            i64::from(session.dedup.contains(&slot.off)),
            rows(Some(&slot.first)),
            rows(slot.later.as_ref()),
            i64::from(view.gone.contains(&slot.off)),
        ])
    }

    /// [`Tap::search`], answered once for each start and projection.
    fn reach(&mut self, from: &[State], want: Key) -> Reached {
        let key = (
            from.iter().map(|s| s.values().copied().collect()).collect(),
            want,
        );
        if let Some(reached) = self.reached.get(&key) {
            return reached.clone();
        }
        let reached = self.search(from, want);
        self.reached.insert(key, reached.clone());
        reached
    }

    fn search(&self, from: &[State], want: Key) -> Reached {
        let mut frontier: Vec<(State, Vec<&'static str>)> =
            from.iter().map(|s| (s.clone(), Vec::new())).collect();
        let mut seen: BTreeSet<Vec<i64>> =
            from.iter().map(|s| s.values().copied().collect()).collect();
        for _ in 0..DEPTH {
            let mut next = Vec::new();
            for (state, path) in &frontier {
                let env = self.model.eval_env(state);
                for (a, &resolved) in self.model.actions.iter().zip(&self.actions) {
                    let action = a.name;
                    let act = &self.model.actions[resolved];
                    for succ in self.model.successors_in(act, &env, state) {
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
    /// the bus: the records whose text `marker` names.
    pub(crate) fn observe(&mut self, view: &View<'_>, marker: &dyn Fn(&str) -> bool) {
        // A text two records hold in this one step opens its slot twice, the later offset's
        // standing: the texts known are those of the slots open before the step.
        let fresh: Vec<Slot> = view
            .records
            .iter()
            .enumerate()
            .filter_map(|(off, record)| {
                let (sid, text) = record.as_ref()?;
                (!self.known.contains(text)
                    && marker(text)
                    && view.sessions.contains_key(sid.as_str()))
                .then(|| Slot {
                    off: off as u64,
                    sid: sid.clone(),
                    first: text.clone(),
                    later: None,
                    gen0: view.generation,
                    retired0: view.retired_logs,
                    forgot0: view.forgot_remints,
                    lost: false,
                    states: initial(&self.model),
                    last: None,
                    done: false,
                })
            })
            .collect();
        for slot in fresh {
            self.opened += 1;
            self.known.insert(slot.first.clone());
            self.slots.insert(slot.first.clone(), slot);
        }
        let mut slots = std::mem::take(&mut self.slots);
        for slot in slots.values_mut().filter(|slot| !slot.done) {
            let had_later = slot.later.is_some();
            let projected = self.project(slot, view, marker);
            if let Some(later) = slot.later.as_ref().filter(|_| !had_later) {
                self.known.insert(later.clone());
            }
            match projected {
                None => slot.done = true,
                Some(want) if slot.last == Some(want) => {}
                Some(want) => {
                    self.checked += 1;
                    if slot.retired0 > 0 {
                        self.checked_past_a_retire += 1;
                    }
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
        self.slots = slots;
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

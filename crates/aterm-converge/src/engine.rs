// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The engine: every item through probe, plan, apply and read-back, in the order its
//! `after` edges give, the items that share an activation read back only after it ran
//! once (design §3.3).

use crate::model::{Explain, ItemReport, Mode, Outcome, Probe, Remedy, Requires, StoodAside};
use crate::scope::Scope;
use crate::{Activation, Item};
use std::fmt;
use std::sync::Arc;

/// Where the engine records a value before it changes it.
pub trait Journal {
    /// Record that `id` held `from` and is about to become `to`; `revert` undoes it.
    ///
    /// # Errors
    /// One line; the engine then does not apply the change.
    fn before_change(
        &mut self,
        id: &str,
        from: &str,
        to: &str,
        revert: Option<&str>,
    ) -> Result<(), String>;
}

/// A journal that keeps nothing: tests, which change nothing real.
#[cfg(any(test, debug_assertions))]
#[derive(Debug, Default)]
pub struct NoJournal;

#[cfg(any(test, debug_assertions))]
impl Journal for NoJournal {
    fn before_change(&mut self, _: &str, _: &str, _: &str, _: Option<&str>) -> Result<(), String> {
        Ok(())
    }
}

/// The journal of a surface that only reads (`aterm pkg doctor`): it refuses every
/// record, so a plan never asks it and an apply that reached a change could not make it.
#[derive(Debug, Default)]
pub struct ReadOnly;

impl Journal for ReadOnly {
    fn before_change(&mut self, id: &str, _: &str, _: &str, _: Option<&str>) -> Result<(), String> {
        Err(format!(
            "this surface changes nothing, so {id} is left as it is"
        ))
    }
}

/// Why a composition of items cannot be run: nothing was probed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// `id`'s `after` names `want`, which no other item of the run is (or, for a `*`
    /// prefix, starts).
    Dangling {
        /// The item whose edge dangles.
        id: String,
        /// The edge, as the item spelled it.
        want: String,
    },
    /// These items wait on each other, or on one that does.
    Cycle {
        /// The items no order can place, in composition order.
        ids: Vec<String>,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dangling { id, want } => {
                write!(f, "{id} runs after {want}, which no item of this run is")
            }
            Self::Cycle { ids } => {
                write!(f, "these items wait on each other: {}", ids.join(", "))
            }
        }
    }
}

impl std::error::Error for Refusal {}

/// Whether an `after` edge `want` names the item `id`.
#[inline]
fn names(want: &str, id: &str) -> bool {
    match want.strip_suffix('*') {
        Some(prefix) => id.starts_with(prefix),
        None => id == want,
    }
}

/// The items of a run (`ids`, in composition order) that the `after` edge `want` of the
/// item at `me` names: the one with that id, or, for a `*` prefix, every other item the
/// prefix starts. One rule for [`order`] and for a surface that keeps only the edges its
/// own composition can meet (`aterm_machines::engine::bind`).
pub fn named_by<'a>(
    want: &'a str,
    me: usize,
    ids: &'a [String],
) -> impl Iterator<Item = usize> + 'a {
    let prefix = want.ends_with('*');
    ids.iter()
        .enumerate()
        .filter(move |&(other, id)| !(prefix && other == me) && names(want, id))
        .map(|(other, _)| other)
}

/// The order `items` run in: each after every item its `after` names, and otherwise in
/// the order given, so a composition that is already ordered keeps its order. An item that
/// waits on an item with an [`Item::activation`] it does not share runs after every item of
/// that activation: that item is read back only once the last of them is done.
///
/// # Errors
/// An edge that names no other item ([`Refusal::Dangling`]; an item that names itself
/// exactly waits on itself, a [`Refusal::Cycle`]), or items that wait on each other.
pub fn order(items: &[Box<dyn Item + '_>]) -> Result<Vec<usize>, Refusal> {
    let ids: Vec<String> = items.iter().map(|item| item.id()).collect();
    let activations: Vec<Option<String>> = items
        .iter()
        .map(|item| item.activation().map(|activation| activation.id()))
        .collect();
    let mut needs: Vec<Vec<usize>> = Vec::with_capacity(items.len());
    for (me, item) in items.iter().enumerate() {
        let mut mine = Vec::new();
        for want in item.after() {
            let before = mine.len();
            mine.extend(named_by(&want, me, &ids));
            if mine.len() == before {
                return Err(Refusal::Dangling {
                    id: ids[me].clone(),
                    want,
                });
            }
        }
        let shared: Vec<&String> = mine
            .iter()
            .filter_map(|&other| activations[other].as_ref())
            .filter(|&activation| activations[me].as_ref() != Some(activation))
            .collect();
        for activation in shared {
            mine.extend(
                (0..items.len()).filter(|&other| {
                    other != me && activations[other].as_ref() == Some(activation)
                }),
            );
        }
        needs.push(mine);
    }
    let mut placed = vec![false; items.len()];
    let mut out = Vec::with_capacity(items.len());
    while out.len() < items.len() {
        let next = (0..items.len()).find(|&i| !placed[i] && needs[i].iter().all(|&j| placed[j]));
        let Some(next) = next else {
            return Err(Refusal::Cycle {
                ids: (0..items.len())
                    .filter(|&i| !placed[i])
                    .map(|i| ids[i].clone())
                    .collect(),
            });
        };
        placed[next] = true;
        out.push(next);
    }
    Ok(out)
}

/// Run `items` in [`order`] and report each, in run order.
///
/// In [`Mode::Apply`] a planned change is journaled, applied, and probed again. Only a
/// read-back that probes `Ok` counts as [`Outcome::Changed`]. Anything else is
/// [`Outcome::Failed`], with the read-back in `why`. A failure does not stop the run.
/// An applied item with an [`Item::activation`] is read back only after that activation
/// ran: once, after the last item of the run that names it, journaled first (its id, and
/// the `from` and `to` its [`Activation::pending`] gives). When nothing is pending it is
/// not run. One that fails, or whose line the journal refuses, fails each item it was to
/// activate, and nothing of it is undone; one whose part for an item failed fails that
/// item alone, and the others are read back.
/// An item whose [`Item::after`] names an earlier item that did not end `Ok` or `Changed`
/// is not probed in an apply run: it is [`Outcome::Blocked`] on that item. In a plan
/// run it is probed as usual, since its prerequisite may only be planned; when that
/// probe [`Probe::Waits`] on an item this plan changes first (git's settings before the
/// Command Line Tools exist), the row is [`Outcome::Planned`]: the apply judges it after
/// that change. Any other block stays [`Outcome::Blocked`], and an unreadable probe stays
/// [`Outcome::Unknown`], whatever is planned before it: a read that failed is a failure,
/// never a plan.
///
/// # Errors
/// The composition cannot be ordered ([`order`]); nothing was probed.
pub fn run(
    items: &[Box<dyn Item + '_>],
    mode: Mode,
    journal: &mut dyn Journal,
) -> Result<Vec<ItemReport>, Refusal> {
    run_with(items, mode, journal, &mut |_| {})
}

/// [`run`], handing each row to `on_row` as soon as its item finishes: the machines
/// agent's run job streams them to `progress.ndjson`.
///
/// # Errors
/// As [`run`].
pub fn run_with(
    items: &[Box<dyn Item + '_>],
    mode: Mode,
    journal: &mut dyn Journal,
    on_row: &mut dyn FnMut(&ItemReport),
) -> Result<Vec<ItemReport>, Refusal> {
    run_scoped(items, mode, Scope::EVERY, journal, on_row)
}

/// [`run_with`] within what a surface allows (design §3.2, [`Scope`]).
///
/// # Errors
/// As [`run`].
pub fn run_scoped(
    items: &[Box<dyn Item + '_>],
    mode: Mode,
    scope: Scope,
    journal: &mut dyn Journal,
    on_row: &mut dyn FnMut(&ItemReport),
) -> Result<Vec<ItemReport>, Refusal> {
    let order = order(items)?;
    // Outside a shell of the account's own (root, another `$HOME`, a private state root),
    // an account item is read and its change named, never made (design §3.2).
    let hold = scope.not_own_account.map(crate::scope::not_own_account);
    let activations: Vec<Option<Arc<dyn Activation + '_>>> =
        items.iter().map(|item| item.activation()).collect();
    // Each activation runs once the last item (in run order) that names it is done.
    let mut last: Vec<(String, usize)> = Vec::new();
    for (position, &index) in order.iter().enumerate() {
        if let Some(activation) = &activations[index] {
            let id = activation.id();
            match last.iter_mut().find(|(seen, _)| *seen == id) {
                Some(entry) => entry.1 = position,
                None => last.push((id, position)),
            }
        }
    }
    // The rows applied and not yet read back: their activation's id, the row, the item.
    let mut awaiting: Vec<(String, usize, usize)> = Vec::new();
    let mut rows: Vec<ItemReport> = Vec::with_capacity(items.len());
    for (position, &index) in order.iter().enumerate() {
        let item = items[index].as_ref();
        let activation = activations[index].as_deref();
        let (row, awaits) =
            if item.requires() == Requires::Console && !scope.admits(Requires::Console) {
                // Where a console item may not run, nothing of it is read (a workstation item
                // over SSH reads another login's session otherwise).
                let why = scope.not_own_account.map_or_else(
                    || crate::scope::NOT_AT_CONSOLE.to_owned(),
                    crate::scope::not_own_account,
                );
                let mut row = blank(item);
                row.outcome = Outcome::Blocked;
                row.explain = Some(item.explain(&Probe::Blocked { why: why.clone() }));
                row.why = Some(why);
                (row, false)
            } else {
                // A class this surface does not apply is planned in an apply run: probed,
                // never applied, and anything after it waits.
                let mode = if mode == Mode::Apply && !scope.applies(item.class()) {
                    Mode::Plan
                } else {
                    mode
                };
                let blocker = (mode == Mode::Apply)
                    .then(|| unmet_prerequisite(item, &rows))
                    .flatten();
                match blocker {
                    // The prerequisite stood aside (a lock, the network): so does what waits
                    // for it, with the same line, so a surface's 75/69 counts it.
                    Some((on, Some(aside))) => {
                        let mut row = blank(item);
                        row.outcome = Outcome::Blocked;
                        row.why = Some(format!("waits for {on}: {}", aside.why()));
                        row.explain = Some(explain_aside(item, &aside));
                        row.stood_aside = Some(aside);
                        (row, false)
                    }
                    Some((on, None)) => {
                        let why = format!("waits for {on}");
                        let mut row = blank(item);
                        row.outcome = Outcome::Blocked;
                        row.explain = Some(item.explain(&Probe::Waits {
                            on,
                            why: why.clone(),
                        }));
                        row.why = Some(why);
                        (row, false)
                    }
                    None => run_one(
                        item,
                        mode,
                        journal,
                        &rows,
                        hold.as_deref(),
                        activation.is_some(),
                    ),
                }
            };
        if awaits && let Some(activation) = activation {
            awaiting.push((activation.id(), rows.len(), index));
        } else {
            on_row(&row);
        }
        rows.push(row);
        if let Some(activation) = activation {
            let id = activation.id();
            if last.iter().any(|(seen, at)| *seen == id && *at == position) {
                let mine: Vec<(usize, usize)> = awaiting
                    .iter()
                    .filter(|(of, _, _)| *of == id)
                    .map(|&(_, at, item)| (at, item))
                    .collect();
                awaiting.retain(|(of, _, _)| *of != id);
                settle(activation, &mine, items, &mut rows, journal, on_row);
            }
        }
    }
    Ok(rows)
}

/// Run `activation` once for the rows `awaiting` it (each a row and its item), its line
/// journaled first, then read each of those items back, but for one whose part of it
/// failed. Nothing awaits it: it is not run.
fn settle(
    activation: &dyn Activation,
    awaiting: &[(usize, usize)],
    items: &[Box<dyn Item + '_>],
    rows: &mut [ItemReport],
    journal: &mut dyn Journal,
    on_row: &mut dyn FnMut(&ItemReport),
) {
    if awaiting.is_empty() {
        return;
    }
    // Whether the journal refused its line, and why it did not run; else the items whose
    // part of it failed. A refused line is explained as a refused item line is
    // (`Blocked`): nothing the items' own remedy runs lifts it, the journal must take the
    // record first.
    let activated: Result<Vec<(String, String)>, (bool, String)> = match activation.pending() {
        Err(why) => Err((false, why)),
        Ok(None) => Ok(Vec::new()),
        Ok(Some((from, to))) => journal
            .before_change(&activation.id(), &from, &to, None)
            .map_err(|why| (true, format!("the journal refused the record ({why})")))
            .and_then(|()| activation.activate().map_err(|why| (false, why))),
    };
    for &(at, index) in awaiting {
        let item = items[index].as_ref();
        let row = &mut rows[at];
        let failed = match &activated {
            Err((refused, why)) => Some((*refused, why.as_str())),
            Ok(failed) => failed
                .iter()
                .find(|(id, _)| *id == row.id)
                .map(|(_, why)| (false, why.as_str())),
        };
        let last = match failed {
            None => read_back(item, row),
            Some((refused, why)) => {
                let why = format!("applied, but not activated: {why}");
                row.outcome = Outcome::Failed;
                row.why = Some(why.clone());
                if refused {
                    Probe::Blocked { why }
                } else {
                    Probe::Unusable { why }
                }
            }
        };
        finish(item, row, &last);
        on_row(row);
    }
}

/// Probe `item` again after its change: only an `Ok` read-back makes `row` `Changed`; any
/// other fails it, saying what the read-back found. The read-back, for the row's
/// explanation.
fn read_back(item: &dyn Item, row: &mut ItemReport) -> Probe {
    let back = item.probe();
    match &back {
        Probe::Ok { .. } => row.outcome = Outcome::Changed,
        Probe::Change { from: now, .. } => {
            row.outcome = Outcome::Failed;
            row.why = Some(format!("applied, but it still reads {now}"));
        }
        Probe::Unusable { why } => {
            row.outcome = Outcome::Failed;
            row.why = Some(format!("applied, but the read-back did not answer: {why}"));
        }
        Probe::Blocked { why }
        | Probe::Waits { why, .. }
        | Probe::NotApplicable { why }
        | Probe::Deferred { why }
        | Probe::Person { how: why, .. } => {
            row.outcome = Outcome::Failed;
            row.why = Some(format!("applied, but the read-back says: {why}"));
        }
    }
    back
}

/// Complete `row` once its outcome is known: the item's detail, and for a row that is not
/// `Ok` or `Changed` its explanation, by the probe `last` that judged it.
fn finish(item: &dyn Item, row: &mut ItemReport, last: &Probe) {
    row.detail = item.detail();
    if !matches!(row.outcome, Outcome::Ok | Outcome::Changed) {
        row.explain = Some(match &row.stood_aside {
            Some(aside) => explain_aside(item, aside),
            None => item.explain(last),
        });
    }
}

/// The first earlier row `item` must wait for that did not end `Ok` or `Changed`, and
/// what it stood aside on, if it did.
fn unmet_prerequisite(
    item: &dyn Item,
    rows: &[ItemReport],
) -> Option<(String, Option<StoodAside>)> {
    for want in item.after() {
        for row in rows.iter().filter(|r| names(&want, &r.id)) {
            if !matches!(row.outcome, Outcome::Ok | Outcome::Changed) {
                return Some((row.id.clone(), row.stood_aside.clone()));
            }
        }
    }
    None
}

/// `item`'s row before anything is known.
fn blank(item: &dyn Item) -> ItemReport {
    ItemReport {
        id: item.id(),
        family: item.family().to_owned(),
        title: item.title(),
        outcome: Outcome::Ok,
        from: None,
        to: None,
        why: None,
        revert: None,
        effect: item.effect(),
        detail: None,
        layer: Some(item.layer()),
        class: Some(item.class()),
        explain: None,
        stood_aside: None,
        essential: item.essential(),
    }
}

/// One item through probe, plan, apply and read-back; `earlier` are the rows before it.
/// With `hold`, the change the item would make is named and `Blocked` with that line, and
/// nothing of it is applied, refreshed or journaled. With `activated`, an apply that did
/// its work is not read back here: the row is returned with `true`, applied (`Changed`, as
/// an item of the same activation that waits on it reads it), for [`settle`] to read back
/// and complete after the activation.
fn run_one(
    item: &dyn Item,
    mode: Mode,
    journal: &mut dyn Journal,
    earlier: &[ItemReport],
    hold: Option<&str>,
    activated: bool,
) -> (ItemReport, bool) {
    let mut row = blank(item);
    // The probe the row's explanation reads: the first, or the read-back that judged it.
    let mut last = item.probe();
    match last.clone() {
        Probe::Ok { now } => {
            row.to = Some(now.clone());
            // A refresh that changes the machine is journaled before it runs, as an apply
            // is ([`Item::refresh_changes`]); a refused record leaves it unrun.
            let recorded = if mode == Mode::Apply && hold.is_none() && item.refresh_changes() {
                journal
                    .before_change(&row.id, &now, &now, item.revert().as_deref())
                    .map_err(|why| format!("not refreshed: the journal refused the record ({why})"))
            } else {
                Ok(())
            };
            if let Err(why) = recorded {
                row.outcome = Outcome::Failed;
                last = Probe::Blocked { why: why.clone() };
                row.why = Some(why);
            } else if mode == Mode::Apply
                && hold.is_none()
                && let Some(refreshed) = item.refresh()
            {
                match refreshed {
                    Err(why) => {
                        row.outcome = Outcome::Failed;
                        // The row is explained by the refresh that failed, never by the
                        // probe that read the value right before it.
                        last = Probe::Unusable { why: why.clone() };
                        row.why = Some(why);
                    }
                    Ok(()) => {
                        let back = item.probe();
                        match &back {
                            Probe::Ok { now } => row.to = Some(now.clone()),
                            Probe::Change { from, .. } => {
                                row.outcome = Outcome::Failed;
                                row.why = Some(format!("refreshed, but it still reads {from}"));
                            }
                            Probe::Unusable { why }
                            | Probe::Blocked { why }
                            | Probe::Waits { why, .. }
                            | Probe::NotApplicable { why }
                            | Probe::Deferred { why }
                            | Probe::Person { how: why, .. } => {
                                row.outcome = Outcome::Failed;
                                row.why = Some(format!("refreshed, but the read-back says: {why}"));
                            }
                        }
                        last = back;
                    }
                }
            }
        }
        Probe::Person { step, how } => {
            row.outcome = Outcome::Person;
            row.to = Some(step);
            row.why = Some(how);
        }
        Probe::Blocked { why } => {
            row.outcome = Outcome::Blocked;
            row.why = Some(why);
        }
        Probe::Waits { on, why } => {
            let planned = earlier
                .iter()
                .any(|r| r.id == on && r.outcome == Outcome::Planned);
            if mode == Mode::Plan && planned {
                row.outcome = Outcome::Planned;
                row.why = Some(format!(
                    "read after {on}, which this plan changes first ({why})"
                ));
            } else {
                row.outcome = Outcome::Blocked;
                row.why = Some(why);
            }
        }
        Probe::NotApplicable { why } => {
            row.outcome = Outcome::NotApplicable;
            row.why = Some(why);
        }
        Probe::Deferred { why } => {
            row.outcome = Outcome::Deferred;
            row.why = Some(why);
        }
        Probe::Unusable { why } => {
            row.outcome = Outcome::Unknown;
            row.why = Some(why);
        }
        Probe::Change { from, to } => {
            row.from = Some(from.clone());
            row.to = Some(to.clone());
            row.revert = item.revert();
            if let Some(why) = hold {
                // Named, never made: not even a plan, since no apply from here makes it.
                row.outcome = Outcome::Blocked;
                last = Probe::Blocked {
                    why: why.to_owned(),
                };
                row.why = Some(why.to_owned());
            } else if mode == Mode::Plan {
                row.outcome = Outcome::Planned;
            } else if let Some(aside) = item.stands_aside() {
                // It stands aside before it acts (`standing_aside`: a lock the change
                // needs is held): no journal line for a change nobody makes.
                row.outcome = Outcome::Blocked;
                row.why = Some(aside.why().to_owned());
                row.stood_aside = Some(aside);
            } else if let Err(why) =
                journal.before_change(&row.id, &from, &to, row.revert.as_deref())
            {
                let why = format!("not applied: the journal refused the record ({why})");
                row.outcome = Outcome::Failed;
                // Nothing the item's own remedy runs lifts it: the journal must take the
                // record first.
                last = Probe::Blocked { why: why.clone() };
                row.why = Some(why);
            } else {
                match item.apply() {
                    Err(why) => {
                        row.outcome = Outcome::Failed;
                        row.why = Some(why);
                    }
                    Ok(()) => {
                        if let Some(aside) = item.stood_aside() {
                            row.outcome = Outcome::Blocked;
                            row.why = Some(aside.why().to_owned());
                            row.stood_aside = Some(aside);
                        } else if activated {
                            row.outcome = Outcome::Changed;
                            return (row, true);
                        } else {
                            last = read_back(item, &mut row);
                        }
                    }
                }
            }
        }
    }
    finish(item, &mut row, &last);
    (row, false)
}

/// A stood-aside row's explanation: the line the apply stood aside with, and the remedy
/// its kind gives. Offline, the next pass once the network is back
/// ([`Remedy::Network`]); a lock or a wait on someone else, the item's own remedy for a
/// change put off to its next run; a missing permission, the item's remedy for that
/// precondition.
fn explain_aside(item: &dyn Item, aside: &StoodAside) -> Explain {
    let why = aside.why().to_owned();
    let remedy = match aside {
        StoodAside::Offline(_) => Remedy::Network,
        StoodAside::Lock(_) | StoodAside::Waiting(_) => {
            item.explain(&Probe::Deferred { why: why.clone() }).remedy
        }
        StoodAside::Permission(_) => item.explain(&Probe::Blocked { why: why.clone() }).remedy,
    };
    Explain { why, remedy }
}

#[cfg(test)]
pub(crate) mod tests {
    //! The machines engine's tests, moved with the engine (design §10 S3: the regression
    //! proof). Each case and assertion is as it was in `aterm-machines/src/engine.rs` at
    //! b6dd34bf6e; only the fixtures changed, because an item now owns its ports and the
    //! engine passes it no `System` and no context.
    use super::*;
    use crate::model::{Class, Effect, Layer};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// The explanation every fixture gives: the probe, by its variant.
    pub(crate) fn explained(probe: &Probe) -> Explain {
        Explain {
            why: format!("{probe:?}"),
            remedy: Remedy::None,
        }
    }

    /// An item whose probes answer from a list, and which counts its probes and applies.
    pub(crate) struct Scripted {
        pub(crate) probes: std::sync::Mutex<Vec<Probe>>,
        pub(crate) probed: Arc<AtomicU32>,
        pub(crate) applies: Arc<AtomicU32>,
        pub(crate) apply_result: Result<(), String>,
        pub(crate) family: &'static str,
        pub(crate) id: &'static str,
    }

    impl Scripted {
        pub(crate) fn new(probes: Vec<Probe>, apply_result: Result<(), String>) -> Self {
            Self {
                probes: std::sync::Mutex::new(probes),
                probed: Arc::new(AtomicU32::new(0)),
                applies: Arc::new(AtomicU32::new(0)),
                apply_result,
                family: "test",
                id: "test.scripted",
            }
        }
    }

    impl Item for Scripted {
        fn id(&self) -> String {
            self.id.into()
        }
        fn family(&self) -> &'static str {
            self.family
        }
        fn layer(&self) -> Layer {
            Layer::Base
        }
        fn class(&self) -> Class {
            Class::Relay
        }
        fn title(&self) -> String {
            "scripted".into()
        }
        fn probe(&self) -> Probe {
            self.probed.fetch_add(1, Ordering::SeqCst);
            let mut p = self.probes.lock().unwrap();
            if p.len() > 1 {
                p.remove(0)
            } else {
                p[0].clone()
            }
        }
        fn apply(&self) -> Result<(), String> {
            self.applies.fetch_add(1, Ordering::SeqCst);
            self.apply_result.clone()
        }
        fn explain(&self, probe: &Probe) -> Explain {
            explained(probe)
        }
        fn revert(&self) -> Option<String> {
            Some("undo".into())
        }
        fn effect(&self) -> Effect {
            Effect::Login
        }
    }

    pub(crate) fn change() -> Probe {
        Probe::Change {
            from: "a".into(),
            to: "b".into(),
        }
    }

    pub(crate) fn ok() -> Probe {
        Probe::Ok { now: "b".into() }
    }

    fn one(item: Scripted, mode: Mode) -> (ItemReport, u32) {
        let applies = Arc::clone(&item.applies);
        let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
        let rows = run(&items, mode, &mut NoJournal).unwrap();
        (
            rows.into_iter().next().unwrap(),
            applies.load(Ordering::SeqCst),
        )
    }

    /// The row carries its item's `essential`, so a reader of the report knows which rows
    /// decide the surface's exit; an item that does not say is advisory.
    #[test]
    fn a_row_says_whether_its_item_is_essential() {
        struct Essential;
        impl Item for Essential {
            fn id(&self) -> String {
                "test.essential".into()
            }
            fn family(&self) -> &'static str {
                "test"
            }
            fn layer(&self) -> Layer {
                Layer::Base
            }
            fn class(&self) -> Class {
                Class::Relay
            }
            fn title(&self) -> String {
                "essential".into()
            }
            fn probe(&self) -> Probe {
                change()
            }
            fn apply(&self) -> Result<(), String> {
                Ok(())
            }
            fn explain(&self, probe: &Probe) -> Explain {
                explained(probe)
            }
            fn essential(&self) -> bool {
                true
            }
        }
        let items: Vec<Box<dyn Item>> = vec![Box::new(Essential)];
        let rows = run(&items, Mode::Plan, &mut ReadOnly).unwrap();
        assert!(rows[0].essential && rows[0].fails_surface(), "{rows:?}");
        let (row, _) = one(Scripted::new(vec![change()], Ok(())), Mode::Plan);
        assert!(!row.essential && !row.fails_surface(), "{row:?}");
    }

    /// A surface that only reads runs on [`ReadOnly`]: a plan never asks it, and an apply
    /// that reached a change could not make it, the record being refused first.
    #[test]
    fn a_read_only_journal_refuses_every_change() {
        let item = Scripted::new(vec![change()], Ok(()));
        let applies = Arc::clone(&item.applies);
        let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
        let plan = run(&items, Mode::Plan, &mut ReadOnly).unwrap();
        assert_eq!(plan[0].outcome, Outcome::Planned);
        let apply = run(&items, Mode::Apply, &mut ReadOnly).unwrap();
        assert_eq!(apply[0].outcome, Outcome::Failed);
        assert!(
            apply[0]
                .why
                .as_deref()
                .is_some_and(|w| w.contains("changes nothing")),
            "{apply:?}"
        );
        assert_eq!(applies.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_plan_run_never_applies() {
        let (row, applies) = one(Scripted::new(vec![change()], Ok(())), Mode::Plan);
        assert_eq!(row.outcome, Outcome::Planned);
        assert_eq!(applies, 0);
        assert_eq!(row.revert.as_deref(), Some("undo"));
        assert_eq!(row.effect, Effect::Login);
    }

    #[test]
    fn only_a_matching_read_back_counts_as_changed() {
        let (row, applies) = one(Scripted::new(vec![change(), ok()], Ok(())), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Changed);
        assert_eq!(applies, 1);
        let (row, _) = one(Scripted::new(vec![change(), change()], Ok(())), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed);
        assert!(row.why.unwrap().contains("still reads a"));
    }

    #[test]
    fn an_apply_error_is_a_failure_with_its_reason() {
        let (row, _) = one(Scripted::new(vec![change()], Err("no".into())), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed);
        assert_eq!(row.why.as_deref(), Some("no"));
    }

    #[test]
    fn an_item_already_as_wanted_is_not_applied() {
        let (row, applies) = one(Scripted::new(vec![ok()], Ok(())), Mode::Apply);
        assert_eq!(row.outcome, Outcome::Ok);
        assert_eq!(applies, 0);
    }

    struct Refreshing {
        inner: Scripted,
        refreshed: Arc<AtomicU32>,
        result: Result<(), String>,
    }

    impl Item for Refreshing {
        fn id(&self) -> String {
            self.inner.id()
        }
        fn family(&self) -> &'static str {
            "refresh-test"
        }
        fn layer(&self) -> Layer {
            Layer::Workstation
        }
        fn class(&self) -> Class {
            Class::Preference
        }
        fn title(&self) -> String {
            "refresh-test".into()
        }
        fn probe(&self) -> Probe {
            self.inner.probe()
        }
        fn apply(&self) -> Result<(), String> {
            self.inner.apply()
        }
        fn refresh(&self) -> Option<Result<(), String>> {
            self.refreshed.fetch_add(1, Ordering::SeqCst);
            Some(self.result.clone())
        }
        fn explain(&self, probe: &Probe) -> Explain {
            explained(probe)
        }
    }

    #[test]
    fn opt_in_refresh_is_apply_only_and_requires_a_matching_second_probe() {
        for (mode, result, second, expected, count) in [
            (Mode::Plan, Ok(()), change(), Outcome::Ok, 0),
            (Mode::Apply, Ok(()), ok(), Outcome::Ok, 1),
            (Mode::Apply, Ok(()), change(), Outcome::Failed, 1),
            (
                Mode::Apply,
                Err("native refused".into()),
                ok(),
                Outcome::Failed,
                1,
            ),
        ] {
            let refreshed = Arc::new(AtomicU32::new(0));
            let item = Refreshing {
                inner: Scripted::new(vec![ok(), second], Ok(())),
                refreshed: Arc::clone(&refreshed),
                result,
            };
            let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
            let rows = run(&items, mode, &mut NoJournal).unwrap();
            assert_eq!(rows[0].outcome, expected);
            assert_eq!(refreshed.load(Ordering::SeqCst), count);
        }
    }

    #[test]
    fn an_unmeasurable_item_is_unknown_never_ok() {
        let (row, applies) = one(
            Scripted::new(
                vec![Probe::Unusable {
                    why: "timeout".into(),
                }],
                Ok(()),
            ),
            Mode::Apply,
        );
        assert_eq!(row.outcome, Outcome::Unknown);
        assert_eq!(applies, 0);
        assert!(row.outcome.is_failure());
    }

    #[test]
    fn a_reading_a_user_could_not_make_stays_unknown_with_its_own_reason() {
        // `here plan` as a user: `socketfilterfw --get…` answers root or not (firewall.rs),
        // so a garbled read is reported as it is, never as a missing privilege.
        let item = Scripted {
            family: "firewall",
            ..Scripted::new(
                vec![Probe::Unusable {
                    why: "socketfilterfw printed nothing".into(),
                }],
                Ok(()),
            )
        };
        let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
        let rows = run(&items, Mode::Plan, &mut NoJournal).unwrap();
        assert_eq!(rows[0].outcome, Outcome::Unknown);
        assert_eq!(
            rows[0].why.as_deref(),
            Some("socketfilterfw printed nothing")
        );
    }

    /// An item with a fixed id, outcome-by-probe, and prerequisites.
    pub(crate) struct Named {
        pub(crate) id: &'static str,
        pub(crate) probe: Probe,
        pub(crate) after: Vec<String>,
    }

    impl Item for Named {
        fn id(&self) -> String {
            self.id.into()
        }
        fn family(&self) -> &'static str {
            "test"
        }
        fn layer(&self) -> Layer {
            Layer::Base
        }
        fn class(&self) -> Class {
            Class::Install
        }
        fn title(&self) -> String {
            self.id.into()
        }
        fn probe(&self) -> Probe {
            self.probe.clone()
        }
        fn apply(&self) -> Result<(), String> {
            Err("apply must not be reached".into())
        }
        fn explain(&self, probe: &Probe) -> Explain {
            explained(probe)
        }
        fn after(&self) -> Vec<String> {
            self.after.clone()
        }
    }

    #[test]
    fn an_item_waits_for_a_prerequisite_that_did_not_land() {
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Named {
                id: "developer.clt",
                probe: Probe::Unusable { why: "x".into() },
                after: vec![],
            }),
            Box::new(Named {
                id: "homebrew.pkg",
                probe: ok(),
                after: vec!["developer.*".into()],
            }),
            Box::new(Named {
                id: "names.computer",
                probe: ok(),
                after: vec![],
            }),
        ];
        let rows = run(&items, Mode::Apply, &mut NoJournal).unwrap();
        assert_eq!(rows[0].outcome, Outcome::Unknown);
        assert_eq!(rows[1].outcome, Outcome::Blocked);
        assert_eq!(rows[1].why.as_deref(), Some("waits for developer.clt"));
        assert_eq!(rows[2].outcome, Outcome::Ok);
        // A plan run probes the dependent anyway.
        let rows = run(&items, Mode::Plan, &mut NoJournal).unwrap();
        assert_eq!(rows[1].outcome, Outcome::Ok);
    }

    #[test]
    fn a_plan_judges_an_item_waiting_on_a_planned_prerequisite_as_planned() {
        let waits = |on: &str| Probe::Waits {
            on: on.into(),
            why: "no git".into(),
        };
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Named {
                id: "developer.clt",
                probe: change(),
                after: vec![],
            }),
            Box::new(Named {
                id: "github.ssh-urls",
                probe: waits("developer.clt"),
                after: vec!["developer.clt".into()],
            }),
            // Waiting on a row the plan made planned the same way.
            Box::new(Named {
                id: "github.after-urls",
                probe: waits("github.ssh-urls"),
                after: vec!["github.ssh-urls".into()],
            }),
            // With no planned prerequisite, a wait is a block.
            Box::new(Named {
                id: "github.other",
                probe: waits("github.absent"),
                after: vec![],
            }),
        ];
        let rows = run(&items, Mode::Plan, &mut NoJournal).unwrap();
        assert_eq!(rows[1].outcome, Outcome::Planned);
        assert!(rows[1].why.as_deref().unwrap().contains("developer.clt"));
        assert_eq!(rows[2].outcome, Outcome::Planned);
        assert_eq!(rows[3].outcome, Outcome::Blocked);
        assert_eq!(rows[3].why.as_deref(), Some("no git"));
    }

    #[test]
    fn a_plan_keeps_a_block_its_prerequisite_does_not_lift() {
        // Homebrew in a prefix another uid owns: installing the Command Line Tools first
        // changes nothing about that, so the plan says blocked, as the apply will.
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Named {
                id: "developer.command-line-tools",
                probe: change(),
                after: vec![],
            }),
            Box::new(Named {
                id: "homebrew.pkg",
                probe: Probe::Blocked {
                    why: "/opt/homebrew belongs to uid 502".into(),
                },
                after: vec!["developer.command-line-tools".into()],
            }),
            Box::new(Named {
                id: "homebrew.formula.jq",
                probe: Probe::Waits {
                    on: "homebrew.pkg".into(),
                    why: "no brew".into(),
                },
                after: vec!["homebrew.pkg".into()],
            }),
        ];
        let rows = run(&items, Mode::Plan, &mut NoJournal).unwrap();
        assert_eq!(rows[0].outcome, Outcome::Planned);
        assert_eq!(rows[1].outcome, Outcome::Blocked);
        assert_eq!(
            rows[1].why.as_deref(),
            Some("/opt/homebrew belongs to uid 502")
        );
        assert_eq!(rows[2].outcome, Outcome::Blocked);
    }

    #[test]
    fn a_plan_keeps_an_unreadable_item_unknown_behind_a_planned_prerequisite() {
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Named {
                id: "developer.clt",
                probe: change(),
                after: vec![],
            }),
            // git's settings file is mode 000: no answer, whatever the CLT will be.
            Box::new(Named {
                id: "github.git",
                probe: Probe::Unusable {
                    why: "~/.gitconfig: Permission denied".into(),
                },
                after: vec!["developer.clt".into()],
            }),
            // Nor does it carry what waits for it into the plan.
            Box::new(Named {
                id: "github.ssh-urls",
                probe: Probe::Waits {
                    on: "github.git".into(),
                    why: "no git".into(),
                },
                after: vec!["github.git".into()],
            }),
        ];
        let rows = run(&items, Mode::Plan, &mut NoJournal).unwrap();
        assert_eq!(rows[0].outcome, Outcome::Planned);
        assert_eq!(rows[1].outcome, Outcome::Unknown);
        assert!(rows[1].outcome.is_failure());
        assert_eq!(rows[2].outcome, Outcome::Blocked);
    }

    #[test]
    fn not_applicable_and_deferred_are_their_own_outcomes() {
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Named {
                id: "a",
                probe: Probe::NotApplicable {
                    why: "intel".into(),
                },
                after: vec![],
            }),
            Box::new(Named {
                id: "b",
                probe: Probe::Deferred {
                    why: "in use".into(),
                },
                after: vec![],
            }),
        ];
        let rows = run(&items, Mode::Apply, &mut NoJournal).unwrap();
        assert_eq!(rows[0].outcome, Outcome::NotApplicable);
        assert_eq!(rows[1].outcome, Outcome::Deferred);
    }

    struct Refusing;
    impl Journal for Refusing {
        fn before_change(
            &mut self,
            _: &str,
            _: &str,
            _: &str,
            _: Option<&str>,
        ) -> Result<(), String> {
            Err("disk full".into())
        }
    }

    #[test]
    fn a_change_the_journal_cannot_record_is_not_applied() {
        let items: Vec<Box<dyn Item>> = vec![Box::new(Scripted::new(vec![change()], Ok(())))];
        let rows = run(&items, Mode::Apply, &mut Refusing).unwrap();
        assert_eq!(rows[0].outcome, Outcome::Failed);
        assert!(rows[0].why.as_deref().unwrap().contains("disk full"));
    }

    // ── the order (design §3.3) ─────────────────────────────────────────────────────────

    /// [`Named`], counting its probes.
    struct Counted {
        named: Named,
        probes: Arc<AtomicU32>,
    }

    impl Item for Counted {
        fn id(&self) -> String {
            self.named.id()
        }
        fn family(&self) -> &'static str {
            self.named.family()
        }
        fn layer(&self) -> Layer {
            self.named.layer()
        }
        fn class(&self) -> Class {
            self.named.class()
        }
        fn title(&self) -> String {
            self.named.title()
        }
        fn probe(&self) -> Probe {
            self.probes.fetch_add(1, Ordering::SeqCst);
            self.named.probe()
        }
        fn apply(&self) -> Result<(), String> {
            self.named.apply()
        }
        fn explain(&self, probe: &Probe) -> Explain {
            explained(probe)
        }
        fn after(&self) -> Vec<String> {
            self.named.after()
        }
    }

    fn counted(
        id: &'static str,
        probe: Probe,
        after: &[&str],
        probes: &Arc<AtomicU32>,
    ) -> Box<dyn Item> {
        Box::new(Counted {
            named: Named {
                id,
                probe,
                after: after.iter().map(|&a| a.to_owned()).collect(),
            },
            probes: Arc::clone(probes),
        })
    }

    #[test]
    fn an_out_of_order_composition_runs_each_prerequisite_first() {
        // Three crates' items, composed dependants first: develop's before base's.
        let probes = Arc::new(AtomicU32::new(0));
        let items = vec![
            counted(
                "develop.workspace-tools@~/aterm",
                ok(),
                &["base.clt", "base.pkg.toolset", "develop.checkout@~/aterm"],
                &probes,
            ),
            counted("develop.checkout@~/aterm", ok(), &["base.*"], &probes),
            counted("names.computer", ok(), &[], &probes),
            counted(
                "base.pkg.toolset",
                Probe::Unusable { why: "x".into() },
                &[],
                &probes,
            ),
            counted("base.clt", ok(), &[], &probes),
        ];
        let rows = run(&items, Mode::Apply, &mut NoJournal).unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "names.computer",
                "base.pkg.toolset",
                "base.clt",
                "develop.checkout@~/aterm",
                "develop.workspace-tools@~/aterm",
            ]
        );
        // The prerequisite ran first, so its failure blocks what waits for it.
        assert_eq!(rows[1].outcome, Outcome::Unknown);
        assert_eq!(rows[3].outcome, Outcome::Blocked);
        assert_eq!(rows[3].why.as_deref(), Some("waits for base.pkg.toolset"));
        assert_eq!(rows[4].outcome, Outcome::Blocked);
        // A composition already in order keeps it: a machines report's rows stay in the
        // order `items::expand` gives.
        assert_eq!(order(&items[2..]).unwrap(), [0, 1, 2]);
    }

    #[test]
    fn a_cycle_is_refused_before_anything_is_probed() {
        let probes = Arc::new(AtomicU32::new(0));
        let items = vec![
            counted("a", change(), &["c"], &probes),
            counted("b", ok(), &[], &probes),
            counted("c", change(), &["a"], &probes),
            // An item behind the cycle cannot be placed either.
            counted("d", ok(), &["c"], &probes),
        ];
        assert_eq!(
            run(&items, Mode::Apply, &mut NoJournal).unwrap_err(),
            Refusal::Cycle {
                ids: vec!["a".into(), "c".into(), "d".into()]
            }
        );
        // An item that names itself exactly waits on itself.
        let alone = vec![counted("a", ok(), &["a"], &probes)];
        assert!(matches!(order(&alone), Err(Refusal::Cycle { .. })));
        assert_eq!(probes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn an_after_that_names_no_item_is_refused_and_a_prefix_must_match_another() {
        let probes = Arc::new(AtomicU32::new(0));
        for (after, items_refused) in [
            ("developer.clt", true),
            ("developer.*", true),
            // A prefix never matches the item that names it.
            ("base.*", true),
            ("names.*", false),
        ] {
            let items = vec![
                counted("base.shell.rc", ok(), &[after], &probes),
                counted("names.computer", ok(), &[], &probes),
            ];
            let result = run(&items, Mode::Plan, &mut NoJournal);
            if items_refused {
                assert_eq!(
                    result.unwrap_err(),
                    Refusal::Dangling {
                        id: "base.shell.rc".into(),
                        want: after.into(),
                    }
                );
            } else {
                assert_eq!(result.unwrap()[0].id, "names.computer");
            }
        }
        // Only the one composition that was not refused probed anything.
        assert_eq!(probes.load(Ordering::SeqCst), 2);
    }

    // ── what a row adds (design §3.3, report) ───────────────────────────────────────────

    /// [`Scripted`] whose apply stands aside, and whose remedy for a change put off to
    /// its next run is its own command.
    struct Aside(Scripted, StoodAside);

    /// What [`Aside`] gives as its own remedy.
    fn retry() -> Remedy {
        Remedy::Command("aterm pkg install --default-set".into())
    }

    fn permission() -> Remedy {
        Remedy::Person {
            pane: "Privacy & Security > Accessibility".into(),
            how: "allow aterm to control account Settings".into(),
        }
    }

    impl Item for Aside {
        fn id(&self) -> String {
            self.0.id()
        }
        fn family(&self) -> &'static str {
            "aterm"
        }
        fn layer(&self) -> Layer {
            Layer::Base
        }
        fn class(&self) -> Class {
            Class::Install
        }
        fn title(&self) -> String {
            self.0.title()
        }
        fn probe(&self) -> Probe {
            self.0.probe()
        }
        fn apply(&self) -> Result<(), String> {
            self.0.apply()
        }
        fn explain(&self, probe: &Probe) -> Explain {
            match probe {
                Probe::Deferred { .. } => Explain {
                    why: format!("{probe:?}"),
                    remedy: retry(),
                },
                Probe::Blocked { .. } => Explain {
                    why: format!("{probe:?}"),
                    remedy: permission(),
                },
                _ => explained(probe),
            }
        }
        fn stood_aside(&self) -> Option<StoodAside> {
            Some(self.1.clone())
        }
    }

    #[test]
    fn a_stood_aside_apply_is_blocked_with_its_kind_and_is_not_read_back() {
        for (aside, status, remedy) in [
            (
                StoodAside::Offline("the index could not be reached".into()),
                69,
                Remedy::Network,
            ),
            (
                StoodAside::Lock("another atpkg run held the store".into()),
                75,
                retry(),
            ),
            (
                StoodAside::Permission("Accessibility permission has not been granted".into()),
                77,
                permission(),
            ),
        ] {
            // The second probe would read Ok: a stood-aside apply must not reach it.
            let item = Scripted::new(vec![change(), ok()], Ok(()));
            let probed = Arc::clone(&item.probed);
            let items: Vec<Box<dyn Item>> = vec![Box::new(Aside(item, aside.clone()))];
            let rows = run(&items, Mode::Apply, &mut NoJournal).unwrap();
            assert_eq!(rows[0].outcome, Outcome::Blocked);
            assert_eq!(rows[0].why.as_deref(), Some(aside.why()));
            assert_eq!(
                rows[0].stood_aside.as_ref().map(StoodAside::exit_status),
                Some(status)
            );
            assert_eq!(probed.load(Ordering::SeqCst), 1, "{aside:?} was read back");
            // Explained by the line it stood aside with, never by the change it planned:
            // offline waits for the network, a lock for the item's own next run,
            // and permission for the item's blocked-precondition remedy.
            assert_eq!(
                rows[0].explain,
                Some(Explain {
                    why: aside.why().into(),
                    remedy,
                })
            );
        }
    }

    #[test]
    fn a_failed_refresh_and_a_refused_record_are_explained_by_what_failed() {
        // The refresh failed after a probe that read the value as wanted: the row is a
        // failure, so its explanation is the failure's, not the Ok read's "nothing to do".
        let item = Refreshing {
            inner: Scripted::new(vec![ok()], Ok(())),
            refreshed: Arc::new(AtomicU32::new(0)),
            result: Err("native refused".into()),
        };
        let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
        let rows = run(&items, Mode::Apply, &mut NoJournal).unwrap();
        assert_eq!(rows[0].outcome, Outcome::Failed);
        assert_eq!(
            rows[0].explain,
            Some(explained(&Probe::Unusable {
                why: "native refused".into()
            }))
        );
        // A record the journal refused: the change was never made, and the row says why.
        let items: Vec<Box<dyn Item>> = vec![Box::new(Scripted::new(vec![change()], Ok(())))];
        let rows = run(&items, Mode::Apply, &mut Refusing).unwrap();
        let why = rows[0].why.clone().unwrap();
        assert!(why.contains("disk full"), "{why}");
        assert_eq!(rows[0].explain, Some(explained(&Probe::Blocked { why })));
    }

    #[test]
    fn every_row_carries_its_layer_and_class_and_only_unfinished_rows_an_explanation() {
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Scripted {
                id: "a.changes",
                ..Scripted::new(vec![change(), ok()], Ok(()))
            }),
            Box::new(Scripted {
                id: "a.ok",
                ..Scripted::new(vec![ok()], Ok(()))
            }),
            Box::new(Scripted {
                id: "a.fails",
                ..Scripted::new(vec![change(), change()], Ok(()))
            }),
            Box::new(Named {
                id: "a.person",
                probe: Probe::Person {
                    step: "fda".into(),
                    how: "turn it on".into(),
                },
                after: vec![],
            }),
        ];
        let rows = run(&items, Mode::Apply, &mut NoJournal).unwrap();
        assert!(rows.iter().all(|r| r.layer == Some(Layer::Base)));
        assert_eq!(rows[0].class, Some(Class::Relay));
        assert_eq!(rows[3].class, Some(Class::Install));
        assert_eq!(
            rows.iter().map(|r| r.explain.is_some()).collect::<Vec<_>>(),
            [false, false, true, true]
        );
        // A failed read-back is explained by that read-back, not the first probe.
        assert_eq!(rows[2].explain, Some(explained(&change())));
        assert_eq!(
            rows[3].explain.as_ref().unwrap().why,
            format!(
                "{:?}",
                Probe::Person {
                    step: "fda".into(),
                    how: "turn it on".into()
                }
            )
        );
    }

    /// What the activation fixtures did, in order: `probe <id>`, `apply <id>`,
    /// `pending <activation>`, `activate <activation>`, `journal <id>`, `row <id>`.
    type Events = Arc<std::sync::Mutex<Vec<String>>>;

    /// An [`Activation`] that records what the engine asks of it.
    struct Relaunch {
        id: &'static str,
        events: Events,
        pending: Result<Option<(String, String)>, String>,
        result: Result<Vec<(String, String)>, String>,
    }

    impl Activation for Relaunch {
        fn id(&self) -> String {
            self.id.into()
        }
        fn pending(&self) -> Result<Option<(String, String)>, String> {
            self.events
                .lock()
                .unwrap()
                .push(format!("pending {}", self.id));
            self.pending.clone()
        }
        fn activate(&self) -> Result<Vec<(String, String)>, String> {
            self.events
                .lock()
                .unwrap()
                .push(format!("activate {}", self.id));
            self.result.clone()
        }
    }

    /// An item whose probes answer from a list (the last repeats), which records its
    /// probes and applies, and which may share an activation.
    struct Writer {
        id: &'static str,
        probes: std::sync::Mutex<Vec<Probe>>,
        apply: Result<(), String>,
        after: Vec<String>,
        activation: Option<Arc<Relaunch>>,
        events: Events,
    }

    impl Writer {
        fn new(id: &'static str, events: &Events, activation: Option<&Arc<Relaunch>>) -> Self {
            Self {
                id,
                probes: std::sync::Mutex::new(vec![change(), ok()]),
                apply: Ok(()),
                after: Vec::new(),
                activation: activation.cloned(),
                events: Arc::clone(events),
            }
        }
    }

    impl Item for Writer {
        fn id(&self) -> String {
            self.id.into()
        }
        fn family(&self) -> &'static str {
            "test"
        }
        fn layer(&self) -> Layer {
            Layer::Workstation
        }
        fn class(&self) -> Class {
            Class::Preference
        }
        fn title(&self) -> String {
            self.id.into()
        }
        fn probe(&self) -> Probe {
            self.events
                .lock()
                .unwrap()
                .push(format!("probe {}", self.id));
            let mut probes = self.probes.lock().unwrap();
            if probes.len() > 1 {
                probes.remove(0)
            } else {
                probes[0].clone()
            }
        }
        fn apply(&self) -> Result<(), String> {
            self.events
                .lock()
                .unwrap()
                .push(format!("apply {}", self.id));
            self.apply.clone()
        }
        fn explain(&self, probe: &Probe) -> Explain {
            explained(probe)
        }
        fn after(&self) -> Vec<String> {
            self.after.clone()
        }
        fn activation(&self) -> Option<Arc<dyn Activation + '_>> {
            self.activation
                .clone()
                .map(|activation| activation as Arc<dyn Activation>)
        }
    }

    /// A journal that records each line's id among the events, refusing those it names.
    struct Recorded {
        events: Events,
        refuses: &'static [&'static str],
    }

    impl Journal for Recorded {
        fn before_change(
            &mut self,
            id: &str,
            _: &str,
            _: &str,
            _: Option<&str>,
        ) -> Result<(), String> {
            if self.refuses.contains(&id) {
                return Err("the journal is full".into());
            }
            self.events.lock().unwrap().push(format!("journal {id}"));
            Ok(())
        }
    }

    fn relaunch(id: &'static str, events: &Events) -> Arc<Relaunch> {
        Arc::new(Relaunch {
            id,
            events: Arc::clone(events),
            pending: Ok(Some(("old".into(), "relaunched".into()))),
            result: Ok(Vec::new()),
        })
    }

    /// Run `items` with a [`Recorded`] journal, recording each row as it is handed out.
    fn recorded(
        items: &[Box<dyn Item + '_>],
        mode: Mode,
        events: &Events,
        refuses: &'static [&'static str],
    ) -> Vec<ItemReport> {
        let mut journal = Recorded {
            events: Arc::clone(events),
            refuses,
        };
        let streamed = Arc::clone(events);
        run_with(items, mode, &mut journal, &mut |row| {
            streamed.lock().unwrap().push(format!("row {}", row.id));
        })
        .unwrap()
    }

    /// The Dock read what `init.desktop` and `init.dock` wrote, and was relaunched after
    /// each; the one relaunched after the first could write what it read at its start
    /// back over the second. Items that share an activation are therefore all applied,
    /// the activation journaled and run once after the last of them, and each read back
    /// only after it, while an item of no activation between them is read back at once.
    /// The rows keep run order; each is handed out when it is complete. Negative control:
    /// reading an activated item back right after its apply (as before) puts `probe a`
    /// before `apply b`.
    #[test]
    fn items_sharing_an_activation_are_applied_before_it_runs_once_and_read_back_after_it() {
        let events = Events::default();
        let dock = relaunch("relaunch.dock", &events);
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Writer::new("a", &events, Some(&dock))),
            Box::new(Writer::new("c", &events, None)),
            Box::new(Writer::new("b", &events, Some(&dock))),
        ];
        let rows = recorded(&items, Mode::Apply, &events, &[]);
        assert_eq!(
            rows.iter()
                .map(|row| (row.id.as_str(), row.outcome))
                .collect::<Vec<_>>(),
            [
                ("a", Outcome::Changed),
                ("c", Outcome::Changed),
                ("b", Outcome::Changed)
            ]
        );
        assert_eq!(
            *events.lock().unwrap(),
            [
                "probe a",
                "journal a",
                "apply a",
                "probe c",
                "journal c",
                "apply c",
                "probe c",
                "row c",
                "probe b",
                "journal b",
                "apply b",
                "pending relaunch.dock",
                "journal relaunch.dock",
                "activate relaunch.dock",
                "probe a",
                "row a",
                "probe b",
                "row b",
            ]
        );
        // A plan reads, and never asks the activation anything.
        let events = Events::default();
        let dock = relaunch("relaunch.dock", &events);
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Writer::new("a", &events, Some(&dock))),
            Box::new(Writer::new("b", &events, Some(&dock))),
        ];
        let rows = recorded(&items, Mode::Plan, &events, &[]);
        assert!(rows.iter().all(|row| row.outcome == Outcome::Planned));
        assert_eq!(
            *events.lock().unwrap(),
            ["probe a", "row a", "probe b", "row b"]
        );
    }

    /// The activation is what makes the change felt, so an activation that fails, cannot
    /// say what awaits it, or whose line the journal refuses (it is then not run) fails
    /// every item it was to activate, with why; their read-back is not taken for success.
    /// A refused line is explained as a refused item line is (`Blocked`, which no remedy
    /// of the item lifts); a failed one as a read that did not answer (`Unusable`, which
    /// the surface's next run may). Negative control: explaining the refusal as
    /// `Unusable` fails the third case.
    #[test]
    fn an_activation_that_does_not_run_fails_every_item_it_was_to_activate() {
        for (pending, result, refuses, ran, refused) in [
            (
                Ok(Some(("old".into(), "new".into()))),
                Err("Dock did not start again".to_owned()),
                &[][..],
                true,
                false,
            ),
            (
                Err("cannot read the marker".to_owned()),
                Ok(Vec::new()),
                &[][..],
                false,
                false,
            ),
            (
                Ok(Some(("old".into(), "new".into()))),
                Ok(Vec::new()),
                &["relaunch.dock"][..],
                false,
                true,
            ),
        ] {
            let events = Events::default();
            let dock = Arc::new(Relaunch {
                id: "relaunch.dock",
                events: Arc::clone(&events),
                pending,
                result,
            });
            let items: Vec<Box<dyn Item>> = vec![
                Box::new(Writer::new("a", &events, Some(&dock))),
                Box::new(Writer::new("b", &events, Some(&dock))),
            ];
            let rows = recorded(&items, Mode::Apply, &events, refuses);
            for row in &rows {
                assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
                let why = row.why.clone().unwrap();
                assert!(why.starts_with("applied, but not activated: "), "{why}");
                let probe = if refused {
                    Probe::Blocked { why }
                } else {
                    Probe::Unusable { why }
                };
                assert_eq!(row.explain, Some(explained(&probe)), "{row:?}");
            }
            let events = events.lock().unwrap();
            assert_eq!(
                events.iter().any(|event| event == "activate relaunch.dock"),
                ran,
                "{events:?}"
            );
            // No read-back after the apply: the row is judged by the activation.
            assert_eq!(events.iter().filter(|event| *event == "probe a").count(), 1);
        }
    }

    /// Each item settles alone: an activation whose part for one item failed (a consumer
    /// of only that item's keys did not start again) fails that item, with why, and reads
    /// the others back, so one consumer's trouble does not unverify, and so roll back, a
    /// change another consumer took. Negative control: failing every awaiting item on any
    /// named failure fails `b`.
    #[test]
    fn an_activation_that_fails_for_one_item_fails_only_that_item() {
        let events = Events::default();
        let dock = Arc::new(Relaunch {
            id: "relaunch.dock",
            events: Arc::clone(&events),
            pending: Ok(Some(("old".into(), "new".into()))),
            result: Ok(vec![(
                "a".into(),
                "NotificationCenter has not started again".into(),
            )]),
        });
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Writer::new("a", &events, Some(&dock))),
            Box::new(Writer::new("b", &events, Some(&dock))),
        ];
        let rows = recorded(&items, Mode::Apply, &events, &[]);
        assert_eq!(
            rows.iter()
                .map(|row| (row.id.as_str(), row.outcome, row.why.as_deref()))
                .collect::<Vec<_>>(),
            [
                (
                    "a",
                    Outcome::Failed,
                    Some("applied, but not activated: NotificationCenter has not started again")
                ),
                ("b", Outcome::Changed, None),
            ]
        );
        let events = events.lock().unwrap();
        // `a` is judged by the activation, `b` by its read-back after it.
        assert_eq!(events.iter().filter(|event| *event == "probe a").count(), 1);
        assert_eq!(events.iter().filter(|event| *event == "probe b").count(), 2);
    }

    /// An activation runs only for a change that was applied: nothing pending, every item
    /// already as wanted, or the one apply failing leaves it unrun and unjournaled; an
    /// item whose apply fails is failed at once, and the others are still activated.
    #[test]
    fn an_activation_runs_only_when_an_applied_item_awaits_it_and_something_is_pending() {
        // Nothing pending: read back at once, no line.
        let events = Events::default();
        let dock = Arc::new(Relaunch {
            id: "relaunch.dock",
            events: Arc::clone(&events),
            pending: Ok(None),
            result: Ok(Vec::new()),
        });
        let items: Vec<Box<dyn Item>> = vec![Box::new(Writer::new("a", &events, Some(&dock)))];
        let rows = recorded(&items, Mode::Apply, &events, &[]);
        assert_eq!(rows[0].outcome, Outcome::Changed);
        assert_eq!(
            *events.lock().unwrap(),
            [
                "probe a",
                "journal a",
                "apply a",
                "pending relaunch.dock",
                "probe a",
                "row a"
            ]
        );
        // Already as wanted, or an apply that failed: nothing awaits it.
        let events = Events::default();
        let dock = relaunch("relaunch.dock", &events);
        let mut wanted = Writer::new("a", &events, Some(&dock));
        wanted.probes = std::sync::Mutex::new(vec![ok()]);
        let mut fails = Writer::new("b", &events, Some(&dock));
        fails.apply = Err("defaults write failed".into());
        let items: Vec<Box<dyn Item>> = vec![Box::new(wanted), Box::new(fails)];
        let rows = recorded(&items, Mode::Apply, &events, &[]);
        assert_eq!(rows[0].outcome, Outcome::Ok);
        assert_eq!(rows[1].outcome, Outcome::Failed);
        assert_eq!(rows[1].why.as_deref(), Some("defaults write failed"));
        assert!(
            !events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event.contains("relaunch.dock")),
            "{:?}",
            events.lock()
        );
        // A failed apply beside an applied one: that one is still activated.
        let events = Events::default();
        let dock = relaunch("relaunch.dock", &events);
        let mut fails = Writer::new("a", &events, Some(&dock));
        fails.apply = Err("defaults write failed".into());
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(fails),
            Box::new(Writer::new("b", &events, Some(&dock))),
        ];
        let rows = recorded(&items, Mode::Apply, &events, &[]);
        assert_eq!(
            rows.iter().map(|row| row.outcome).collect::<Vec<_>>(),
            [Outcome::Failed, Outcome::Changed]
        );
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| *event == "activate relaunch.dock")
                .count(),
            1
        );
    }

    /// An item a run selects (`--only`, [`crate::select`]) keeps its activation: the two
    /// selected items of one activation still apply first and run it once, after both.
    /// Negative control: `Narrowed` not forwarding [`Item::activation`] reads each back at
    /// once after its own apply, and never runs the activation.
    #[test]
    fn a_selected_item_keeps_its_activation() {
        let events = Events::default();
        let dock = relaunch("relaunch.dock", &events);
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Writer::new("a", &events, Some(&dock))),
            Box::new(Writer::new("c", &events, None)),
            Box::new(Writer::new("b", &events, Some(&dock))),
        ];
        let (items, unmatched) = crate::select(items, &["a".into(), "b".into()]);
        assert!(unmatched.is_empty(), "{unmatched:?}");
        let rows = recorded(&items, Mode::Apply, &events, &[]);
        assert!(
            rows.iter().all(|row| row.outcome == Outcome::Changed),
            "{rows:?}"
        );
        assert_eq!(
            *events.lock().unwrap(),
            [
                "probe a",
                "journal a",
                "apply a",
                "probe b",
                "journal b",
                "apply b",
                "pending relaunch.dock",
                "journal relaunch.dock",
                "activate relaunch.dock",
                "probe a",
                "row a",
                "probe b",
                "row b",
            ]
        );
    }

    /// An item that waits on an activated item runs once that item is read back: after
    /// every item of its activation, whatever the composition order. An item of the same
    /// activation that waits on another applies after it, reading it as applied, and both
    /// are read back after the activation.
    #[test]
    fn an_item_waiting_on_an_activated_item_runs_after_its_whole_activation() {
        let events = Events::default();
        let dock = relaunch("relaunch.dock", &events);
        let mut d = Writer::new("d", &events, None);
        d.after = vec!["a".into()];
        let mut b = Writer::new("b", &events, Some(&dock));
        b.after = vec!["a".into()];
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Writer::new("a", &events, Some(&dock))),
            Box::new(d),
            Box::new(b),
        ];
        assert_eq!(order(&items).unwrap(), [0, 2, 1]);
        let rows = recorded(&items, Mode::Apply, &events, &[]);
        assert_eq!(
            rows.iter()
                .map(|row| (row.id.as_str(), row.outcome))
                .collect::<Vec<_>>(),
            [
                ("a", Outcome::Changed),
                ("b", Outcome::Changed),
                ("d", Outcome::Changed)
            ]
        );
        let events = events.lock().unwrap();
        let at = |event: &str| events.iter().position(|e| e == event).unwrap();
        assert!(at("apply b") < at("activate relaunch.dock"));
        assert!(at("activate relaunch.dock") < at("probe d"));
        // A dependant of an activation that fails waits, and is not applied.
        let events = Events::default();
        let dock = Arc::new(Relaunch {
            id: "relaunch.dock",
            events: Arc::clone(&events),
            pending: Ok(Some(("old".into(), "new".into()))),
            result: Err("Dock did not start again".into()),
        });
        let mut d = Writer::new("d", &events, None);
        d.after = vec!["a".into()];
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Writer::new("a", &events, Some(&dock))),
            Box::new(d),
        ];
        let rows = recorded(&items, Mode::Apply, &events, &[]);
        assert_eq!(rows[1].outcome, Outcome::Blocked);
        assert!(
            !events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event == "apply d")
        );
    }
}

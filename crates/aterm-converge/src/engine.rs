// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Convergence engine ordering and executing items through probe, plan, apply, and read-back.

use crate::model::{
    Explain, ItemReport, Mode, Outcome, Probe, Remedy, Requires, StoodAside, millis,
};
use crate::scope::Scope;
use crate::{Activation, Item};
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

/// Where the engine records a value before it changes it.
pub trait Journal {
    /// Record that `id` held `from` and is about to become `to`; `revert` undoes it.
    /// # Errors
    /// One line; the engine then does not apply the change.
    fn before_change(
        &mut self,
        id: &str,
        from: &str,
        to: &str,
        revert: Option<&str>,
    ) -> Result<(), String>;

    /// Record that `id` ended failed in an apply run, and why: its row's why. A journal that keeps
    /// changes alone records nothing.
    /// # Errors
    /// One line; the row ends failed whether or not it was recorded.
    fn failed(&mut self, id: &str, why: &str) -> Result<(), String> {
        let _ = (id, why);
        Ok(())
    }
}

/// Record `row` in `journal` where it ended failed. A record the journal refuses is let go: the
/// row, the run's report and its record say the failure all the same.
fn journal_failure(journal: &mut dyn Journal, row: &ItemReport) {
    if row.outcome == Outcome::Failed {
        let _ = journal.failed(&row.id, row.why.as_deref().unwrap_or("no reason given"));
    }
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

/// Read-only journal that refuses every write.
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
    /// `id`'s `after` names `want`, which no other item of the run is (or, for a `*` prefix, starts).
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

/// The items of a run (`ids`, in composition order) that the `after` edge `want` of the item at `me` names: the one with that id, or, for a `*` prefix, every other item the prefix starts.
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

/// Return a topological execution order for `items` preserving composition order.
/// # Errors
/// [`Refusal::Dangling`] for an unknown dependency or [`Refusal::Cycle`] for a dependency cycle.
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
/// # Errors
/// The composition cannot be ordered ([`order`]); nothing was probed.
pub fn run(
    items: &[Box<dyn Item + '_>],
    mode: Mode,
    journal: &mut dyn Journal,
) -> Result<Vec<ItemReport>, Refusal> {
    run_with(items, mode, journal, &mut |_| {})
}

/// Run `items` in [`order`] and pass each completed row to `on_row`.
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

/// Run `items` within `scope` and pass each completed row to `on_row`. Every row carries
/// the time the engine spent on it ([`ItemReport::elapsed_ms`]) by the time `on_row` sees it.
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
    let hold = scope.not_own_account.map(crate::scope::not_own_account);
    let activations: Vec<Option<Arc<dyn Activation + '_>>> =
        items.iter().map(|item| item.activation()).collect();
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
    let mut awaiting: Vec<(String, usize, usize)> = Vec::new();
    let mut passive: Vec<(String, usize, usize)> = Vec::new();
    let mut rows: Vec<ItemReport> = Vec::with_capacity(items.len());
    for (position, &index) in order.iter().enumerate() {
        let item = items[index].as_ref();
        let started = Instant::now();
        let activation = activations[index].as_deref();
        let item_mode = if mode == Mode::Apply && !scope.applies(item.class()) {
            Mode::Plan
        } else {
            mode
        };
        let (mut row, awaits) =
            if item.requires() == Requires::Console && !scope.admits(Requires::Console) {
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
                let blocker = (item_mode == Mode::Apply)
                    .then(|| unmet_prerequisite(item, &rows))
                    .flatten();
                match blocker {
                    Some((on, Some((aside, origin)))) => {
                        let mut row = blank(item);
                        row.outcome = Outcome::Blocked;
                        row.why = Some(format!("waits for {on}: {}", aside.why()));
                        row.explain = Some(explain_aside(item, &aside));
                        row.stood_aside = Some(aside);
                        row.waits_on = Some(origin);
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
                        item_mode,
                        journal,
                        &rows,
                        hold.as_deref(),
                        activation.is_some(),
                    ),
                }
            };
        row.elapsed_ms = Some(millis(started.elapsed()));
        if item_mode == Mode::Apply {
            journal_failure(journal, &row);
        }
        if awaits && let Some(activation) = activation {
            awaiting.push((activation.id(), rows.len(), index));
        } else if item_mode == Mode::Apply
            && hold.is_none()
            && row.outcome == Outcome::Ok
            && let Some(activation) = activation
        {
            // Another item can relaunch this item's reader after its successful probe.
            // Only an actual activation needs another read; this item changes nothing.
            passive.push((activation.id(), rows.len(), index));
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
                let unchanged: Vec<(usize, usize)> = passive
                    .iter()
                    .filter(|(of, _, _)| *of == id)
                    .map(|&(_, at, item)| (at, item))
                    .collect();
                passive.retain(|(of, _, _)| *of != id);
                settle(
                    activation, &mine, &unchanged, items, &mut rows, journal, on_row,
                );
            }
        }
    }
    Ok(rows)
}

/// Run `activation` once for the rows `awaiting` it, then read back those rows and the
/// `passive` ones. Each row's read-back adds to its time, and the activation's own time is
/// charged once, to the first awaiting row, so a report's rows never count it twice.
fn settle(
    activation: &dyn Activation,
    awaiting: &[(usize, usize)],
    passive: &[(usize, usize)],
    items: &[Box<dyn Item + '_>],
    rows: &mut [ItemReport],
    journal: &mut dyn Journal,
    on_row: &mut dyn FnMut(&ItemReport),
) {
    let mut invoked = false;
    let started = Instant::now();
    let activated: Result<Vec<(String, String)>, (bool, String)> = if awaiting.is_empty() {
        Ok(Vec::new())
    } else {
        match activation.pending() {
            Err(why) => Err((false, why)),
            Ok(None) => Ok(Vec::new()),
            Ok(Some((from, to))) => journal
                .before_change(&activation.id(), &from, &to, None)
                .map_err(|why| (true, format!("the journal refused the record ({why})")))
                .and_then(|()| {
                    invoked = true;
                    activation.activate().map_err(|why| (false, why))
                }),
        }
    };
    let mut shared = Some(started.elapsed());
    let mut completing: Vec<_> = awaiting
        .iter()
        .map(|&(at, index)| (at, index, false))
        .chain(passive.iter().map(|&(at, index)| (at, index, true)))
        .collect();
    completing.sort_unstable_by_key(|&(at, _, _)| at);
    for (at, index, passive) in completing {
        let item = items[index].as_ref();
        let row = &mut rows[at];
        let started = Instant::now();
        if passive {
            if invoked {
                let last = read_back_as(item, row, Outcome::Ok, "rechecked after activation");
                finish(item, row, &last);
                journal_failure(journal, row);
            }
            add_elapsed(row, started.elapsed());
            on_row(row);
            continue;
        }
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
        journal_failure(journal, row);
        add_elapsed(
            row,
            started
                .elapsed()
                .saturating_add(shared.take().unwrap_or_default()),
        );
        on_row(row);
    }
}

fn add_elapsed(row: &mut ItemReport, took: std::time::Duration) {
    row.elapsed_ms = Some(row.elapsed_ms.unwrap_or(0).saturating_add(millis(took)));
}

fn read_back(item: &dyn Item, row: &mut ItemReport) -> Probe {
    read_back_as(item, row, Outcome::Changed, "applied")
}

fn read_back_as(item: &dyn Item, row: &mut ItemReport, success: Outcome, after: &str) -> Probe {
    let back = item.probe();
    match &back {
        Probe::Ok { now } => {
            row.outcome = success;
            row.to = Some(now.clone());
        }
        Probe::Change { from: now, .. } => {
            row.outcome = Outcome::Failed;
            row.why = Some(format!("{after}, but it still reads {now}"));
        }
        Probe::Unusable { why } => {
            row.outcome = Outcome::Failed;
            row.why = Some(format!("{after}, but the read-back did not answer: {why}"));
        }
        Probe::Blocked { why }
        | Probe::Waits { why, .. }
        | Probe::NotApplicable { why }
        | Probe::Deferred { why }
        | Probe::Person { how: why, .. } => {
            row.outcome = Outcome::Failed;
            row.why = Some(format!("{after}, but the read-back says: {why}"));
        }
    }
    back
}

fn finish(item: &dyn Item, row: &mut ItemReport, last: &Probe) {
    row.detail = item.detail();
    if !matches!(row.outcome, Outcome::Ok | Outcome::Changed) {
        row.explain = Some(match &row.stood_aside {
            Some(aside) => explain_aside(item, aside),
            None => item.explain(last),
        });
    }
}

/// The first row `item` waits for ([`Item::after`]) that did not end `Ok` or `Changed`: its id,
/// and, where it stood aside, its stand-aside with the row that stand-aside comes from (the row
/// itself, or the one it in turn inherited it from, [`ItemReport::waits_on`]).
fn unmet_prerequisite(
    item: &dyn Item,
    rows: &[ItemReport],
) -> Option<(String, Option<(StoodAside, String)>)> {
    for want in item.after() {
        for row in rows.iter().filter(|r| names(&want, &r.id)) {
            if !matches!(row.outcome, Outcome::Ok | Outcome::Changed) {
                let origin = row.waits_on.clone().unwrap_or_else(|| row.id.clone());
                return Some((
                    row.id.clone(),
                    row.stood_aside.clone().map(|aside| (aside, origin)),
                ));
            }
        }
    }
    None
}

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
        waits_on: None,
        essential: item.essential(),
        elapsed_ms: None,
    }
}

fn run_one(
    item: &dyn Item,
    mode: Mode,
    journal: &mut dyn Journal,
    earlier: &[ItemReport],
    hold: Option<&str>,
    activated: bool,
) -> (ItemReport, bool) {
    let mut row = blank(item);
    let mut last = item.probe();
    match last.clone() {
        Probe::Ok { now } => {
            row.to = Some(now.clone());
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
                        last = Probe::Unusable { why: why.clone() };
                        row.why = Some(why);
                    }
                    // A refresh that stood aside did not run: blocked with its kind, as a
                    // stood-aside apply is, and not read back.
                    Ok(()) if let Some(aside) = item.stood_aside() => {
                        row.outcome = Outcome::Blocked;
                        row.why = Some(aside.why().to_owned());
                        row.stood_aside = Some(aside);
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
        Probe::Unusable { why } => match item.read_aside() {
            Some(aside) => {
                row.outcome = Outcome::Blocked;
                row.why = Some(aside.why().to_owned());
                row.stood_aside = Some(aside);
            }
            None => {
                row.outcome = Outcome::Unknown;
                row.why = Some(why);
            }
        },
        Probe::Change { from, to } => {
            row.from = Some(from.clone());
            row.to = Some(to.clone());
            row.revert = item.revert();
            if let Some(why) = hold {
                row.outcome = Outcome::Blocked;
                last = Probe::Blocked {
                    why: why.to_owned(),
                };
                row.why = Some(why.to_owned());
            } else if mode == Mode::Plan {
                row.outcome = Outcome::Planned;
            } else if let Some(aside) = item.stands_aside() {
                row.outcome = Outcome::Blocked;
                row.why = Some(aside.why().to_owned());
                row.stood_aside = Some(aside);
            } else if let Err(why) =
                journal.before_change(&row.id, &from, &to, row.revert.as_deref())
            {
                let why = format!("not applied: the journal refused the record ({why})");
                row.outcome = Outcome::Failed;
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

fn explain_aside(item: &dyn Item, aside: &StoodAside) -> Explain {
    let why = aside.why().to_owned();
    let remedy = match aside {
        StoodAside::Offline(_) => Remedy::Network,
        StoodAside::Lock(_) | StoodAside::Waiting(_) | StoodAside::Absent(_) => {
            item.explain(&Probe::Deferred { why: why.clone() }).remedy
        }
        StoodAside::Permission(_) => item.explain(&Probe::Blocked { why: why.clone() }).remedy,
    };
    Explain { why, remedy }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
pub(crate) mod tests;

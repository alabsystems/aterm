// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The engine: every item through probe, plan, apply and read-back, in the order its
//! `after` edges give (design §3.3).

use crate::Item;
use crate::model::{Explain, ItemReport, Mode, Outcome, Probe, Remedy, StoodAside};
use std::fmt;

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
/// the order given, so a composition that is already ordered keeps its order.
///
/// # Errors
/// An edge that names no other item ([`Refusal::Dangling`]; an item that names itself
/// exactly waits on itself, a [`Refusal::Cycle`]), or items that wait on each other.
pub fn order(items: &[Box<dyn Item + '_>]) -> Result<Vec<usize>, Refusal> {
    let ids: Vec<String> = items.iter().map(|item| item.id()).collect();
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
    let order = order(items)?;
    let mut rows: Vec<ItemReport> = Vec::with_capacity(items.len());
    for index in order {
        let item = items[index].as_ref();
        let blocker = (mode == Mode::Apply)
            .then(|| unmet_prerequisite(item, &rows))
            .flatten();
        let row = match blocker {
            Some(on) => {
                let why = format!("waits for {on}");
                let mut row = blank(item);
                row.outcome = Outcome::Blocked;
                row.explain = Some(item.explain(&Probe::Waits {
                    on,
                    why: why.clone(),
                }));
                row.why = Some(why);
                row
            }
            None => run_one(item, mode, journal, &rows),
        };
        on_row(&row);
        rows.push(row);
    }
    Ok(rows)
}

/// The first earlier row `item` must wait for that did not end `Ok` or `Changed`.
fn unmet_prerequisite(item: &dyn Item, rows: &[ItemReport]) -> Option<String> {
    for want in item.after() {
        for row in rows.iter().filter(|r| names(&want, &r.id)) {
            if !matches!(row.outcome, Outcome::Ok | Outcome::Changed) {
                return Some(row.id.clone());
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
fn run_one(
    item: &dyn Item,
    mode: Mode,
    journal: &mut dyn Journal,
    earlier: &[ItemReport],
) -> ItemReport {
    let mut row = blank(item);
    // The probe the row's explanation reads: the first, or the read-back that judged it.
    let mut last = item.probe();
    match last.clone() {
        Probe::Ok { now } => {
            row.to = Some(now);
            if mode == Mode::Apply
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
            if mode == Mode::Plan {
                row.outcome = Outcome::Planned;
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
                        } else {
                            let back = item.probe();
                            match &back {
                                Probe::Ok { .. } => row.outcome = Outcome::Changed,
                                Probe::Change { from: now, .. } => {
                                    row.outcome = Outcome::Failed;
                                    row.why = Some(format!("applied, but it still reads {now}"));
                                }
                                Probe::Unusable { why } => {
                                    row.outcome = Outcome::Failed;
                                    row.why = Some(format!(
                                        "applied, but the read-back did not answer: {why}"
                                    ));
                                }
                                Probe::Blocked { why }
                                | Probe::Waits { why, .. }
                                | Probe::NotApplicable { why }
                                | Probe::Deferred { why }
                                | Probe::Person { how: why, .. } => {
                                    row.outcome = Outcome::Failed;
                                    row.why =
                                        Some(format!("applied, but the read-back says: {why}"));
                                }
                            }
                            last = back;
                        }
                    }
                }
            }
        }
    }
    row.detail = item.detail();
    if !matches!(row.outcome, Outcome::Ok | Outcome::Changed) {
        row.explain = Some(match &row.stood_aside {
            Some(aside) => explain_aside(item, aside),
            None => item.explain(&last),
        });
    }
    row
}

/// A stood-aside row's explanation: the line the apply stood aside with, and the remedy
/// its kind gives. Offline, the next pass once the network is back
/// ([`Remedy::Network`]); a lock, the item's own remedy for a change put off to its next
/// run.
fn explain_aside(item: &dyn Item, aside: &StoodAside) -> Explain {
    let why = aside.why().to_owned();
    let remedy = match aside {
        StoodAside::Offline(_) => Remedy::Network,
        StoodAside::Lock(_) => item.explain(&Probe::Deferred { why: why.clone() }).remedy,
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
            // offline waits for the network, a lock for the item's own next run.
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
}

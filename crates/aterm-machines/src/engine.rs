// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The engine: every item through probe, plan, apply, verify (design §5).

use crate::items::{Ctx, Item};
use crate::model::{ItemReport, Mode, Outcome, Probe};
use crate::port::System;

/// Where the engine records a value before it changes it.
pub trait Journal {
    /// Record that `id` held `from` and is about to become `to`.
    ///
    /// # Errors
    /// One line; the engine then does not apply the change.
    fn before_change(&mut self, id: &str, from: &str, to: &str) -> Result<(), String>;
}

/// A journal that keeps nothing (plan runs, tests).
#[derive(Debug, Default)]
pub struct NoJournal;

impl Journal for NoJournal {
    fn before_change(&mut self, _id: &str, _from: &str, _to: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Run `items` in order and report each.
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
pub fn run(
    items: &[Box<dyn Item>],
    mode: Mode,
    sys: &dyn System,
    ctx: &Ctx,
    journal: &mut dyn Journal,
) -> Vec<ItemReport> {
    run_with(items, mode, sys, ctx, journal, &mut |_| {})
}

/// [`run`], handing each row to `on_row` as soon as its item finishes: the agent's run job
/// streams them to `progress.ndjson` (design §4.4).
pub fn run_with(
    items: &[Box<dyn Item>],
    mode: Mode,
    sys: &dyn System,
    ctx: &Ctx,
    journal: &mut dyn Journal,
    on_row: &mut dyn FnMut(&ItemReport),
) -> Vec<ItemReport> {
    let mut rows: Vec<ItemReport> = Vec::with_capacity(items.len());
    for item in items {
        let blocker = (mode == Mode::Apply)
            .then(|| unmet_prerequisite(item.as_ref(), &rows))
            .flatten();
        let row = match blocker {
            Some(on) => ItemReport {
                id: item.id(),
                family: item.family().to_owned(),
                title: item.title(),
                outcome: Outcome::Blocked,
                from: None,
                to: None,
                why: Some(format!("waits for {on}")),
                revert: None,
                effect: item.effect(),
                detail: None,
            },
            None => run_one(item.as_ref(), mode, sys, ctx, journal, &rows),
        };
        on_row(&row);
        rows.push(row);
    }
    rows
}

/// The first earlier row `item` must wait for that did not end `Ok` or `Changed`.
fn unmet_prerequisite(item: &dyn Item, rows: &[ItemReport]) -> Option<String> {
    for want in item.after() {
        let matches = |id: &str| match want.strip_suffix('*') {
            Some(prefix) => id.starts_with(prefix),
            None => id == want,
        };
        for row in rows.iter().filter(|r| matches(&r.id)) {
            if !matches!(row.outcome, Outcome::Ok | Outcome::Changed) {
                return Some(row.id.clone());
            }
        }
    }
    None
}

/// One item through probe, plan, apply and read-back; `earlier` are the rows before it.
fn run_one(
    item: &dyn Item,
    mode: Mode,
    sys: &dyn System,
    ctx: &Ctx,
    journal: &mut dyn Journal,
    earlier: &[ItemReport],
) -> ItemReport {
    let mut row = ItemReport {
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
    };
    match item.probe(sys, ctx) {
        Probe::Ok { now } => {
            row.to = Some(now);
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
            } else if let Err(why) = journal.before_change(&row.id, &from, &to) {
                row.outcome = Outcome::Failed;
                row.why = Some(format!(
                    "not applied: the journal refused the record ({why})"
                ));
            } else {
                match item.apply(sys, ctx) {
                    Err(why) => {
                        row.outcome = Outcome::Failed;
                        row.why = Some(why);
                    }
                    Ok(()) if item.stood_aside().is_some() => {
                        row.outcome = Outcome::Blocked;
                        row.why = item.stood_aside();
                    }
                    Ok(()) => match item.probe(sys, ctx) {
                        Probe::Ok { .. } => row.outcome = Outcome::Changed,
                        Probe::Change { from: now, .. } => {
                            row.outcome = Outcome::Failed;
                            row.why = Some(format!("applied, but it still reads {now}"));
                        }
                        Probe::Unusable { why } => {
                            row.outcome = Outcome::Failed;
                            row.why =
                                Some(format!("applied, but the read-back did not answer: {why}"));
                        }
                        Probe::Blocked { why }
                        | Probe::Waits { why, .. }
                        | Probe::NotApplicable { why }
                        | Probe::Deferred { why }
                        | Probe::Person { how: why, .. } => {
                            row.outcome = Outcome::Failed;
                            row.why = Some(format!("applied, but the read-back says: {why}"));
                        }
                    },
                }
            }
        }
    }
    row.detail = item.detail(sys, ctx);
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Effect, HostFacts};
    use crate::port::Fake;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// An item whose probes answer from a list, and which counts its applies.
    struct Scripted {
        probes: std::sync::Mutex<Vec<Probe>>,
        applies: Arc<AtomicU32>,
        apply_result: Result<(), String>,
    }

    impl Scripted {
        fn new(probes: Vec<Probe>, apply_result: Result<(), String>) -> Self {
            Self {
                probes: std::sync::Mutex::new(probes),
                applies: Arc::new(AtomicU32::new(0)),
                apply_result,
            }
        }
    }

    impl Item for Scripted {
        fn id(&self) -> String {
            "test.scripted".into()
        }
        fn family(&self) -> &'static str {
            "test"
        }
        fn title(&self) -> String {
            "scripted".into()
        }
        fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
            let mut p = self.probes.lock().unwrap();
            if p.len() > 1 {
                p.remove(0)
            } else {
                p[0].clone()
            }
        }
        fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
            self.applies.fetch_add(1, Ordering::SeqCst);
            self.apply_result.clone()
        }
        fn revert(&self) -> Option<String> {
            Some("undo".into())
        }
        fn effect(&self) -> Effect {
            Effect::Login
        }
    }

    fn ctx() -> Ctx {
        Ctx {
            facts: HostFacts::default(),
            users: Vec::new(),
            console: None,
            host_name: "h".into(),
            cache_dir: "/c".into(),
            disrupt: true,
        }
    }

    fn change() -> Probe {
        Probe::Change {
            from: "a".into(),
            to: "b".into(),
        }
    }

    fn ok() -> Probe {
        Probe::Ok { now: "b".into() }
    }

    fn one(item: Scripted, mode: Mode) -> (ItemReport, u32) {
        let applies = Arc::clone(&item.applies);
        let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
        let rows = run(&items, mode, &Fake::root(), &ctx(), &mut NoJournal);
        (
            rows.into_iter().next().unwrap(),
            applies.load(Ordering::SeqCst),
        )
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

    /// An item with a fixed id, outcome-by-probe, and prerequisites.
    struct Named {
        id: &'static str,
        probe: Probe,
        after: Vec<String>,
    }

    impl Item for Named {
        fn id(&self) -> String {
            self.id.into()
        }
        fn family(&self) -> &'static str {
            "test"
        }
        fn title(&self) -> String {
            self.id.into()
        }
        fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
            self.probe.clone()
        }
        fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
            Err("apply must not be reached".into())
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
        let rows = run(&items, Mode::Apply, &Fake::root(), &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Unknown);
        assert_eq!(rows[1].outcome, Outcome::Blocked);
        assert_eq!(rows[1].why.as_deref(), Some("waits for developer.clt"));
        assert_eq!(rows[2].outcome, Outcome::Ok);
        // A plan run probes the dependent anyway.
        let rows = run(&items, Mode::Plan, &Fake::root(), &ctx(), &mut NoJournal);
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
        let rows = run(&items, Mode::Plan, &Fake::root(), &ctx(), &mut NoJournal);
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
        let rows = run(&items, Mode::Plan, &Fake::root(), &ctx(), &mut NoJournal);
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
        let rows = run(&items, Mode::Plan, &Fake::root(), &ctx(), &mut NoJournal);
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
        let rows = run(&items, Mode::Apply, &Fake::root(), &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::NotApplicable);
        assert_eq!(rows[1].outcome, Outcome::Deferred);
    }

    struct Refusing;
    impl Journal for Refusing {
        fn before_change(&mut self, _: &str, _: &str, _: &str) -> Result<(), String> {
            Err("disk full".into())
        }
    }

    #[test]
    fn a_change_the_journal_cannot_record_is_not_applied() {
        let items: Vec<Box<dyn Item>> = vec![Box::new(Scripted::new(vec![change()], Ok(())))];
        let rows = run(&items, Mode::Apply, &Fake::root(), &ctx(), &mut Refusing);
        assert_eq!(rows[0].outcome, Outcome::Failed);
        assert!(rows[0].why.as_deref().unwrap().contains("disk full"));
    }
}

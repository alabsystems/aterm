// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::NoJournal;
use crate::engine::tests::{change, explained, ok};
use crate::model::{Mode, Outcome};
use crate::run_scoped;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

struct Placed {
    id: &'static str,
    family: &'static str,
    layer: Layer,
    class: Class,
    requires: Requires,
    after: Vec<String>,
    probes: Mutex<Vec<Probe>>,
    probed: Arc<AtomicU32>,
    applied: Arc<AtomicU32>,
    /// `Some(changes)`: it refreshes over an `Ok` probe, and says whether that changes
    /// the machine ([`Item::refresh_changes`]).
    refresh: Option<bool>,
    refreshed: Arc<AtomicU32>,
}

impl Placed {
    fn new(id: &'static str, class: Class, requires: Requires) -> Self {
        Self {
            id,
            family: "test",
            layer: Layer::Base,
            class,
            requires,
            after: Vec::new(),
            probes: Mutex::new(vec![change(), ok()]),
            probed: Arc::new(AtomicU32::new(0)),
            applied: Arc::new(AtomicU32::new(0)),
            refresh: None,
            refreshed: Arc::new(AtomicU32::new(0)),
        }
    }
}

struct Counted {
    lines: Vec<(String, String, String)>,
    refreshed_at_line: Vec<u32>,
    refreshed: Arc<AtomicU32>,
    refuses: bool,
}

impl Counted {
    fn new(refreshed: &Arc<AtomicU32>) -> Self {
        Self {
            lines: Vec::new(),
            refreshed_at_line: Vec::new(),
            refreshed: Arc::clone(refreshed),
            refuses: false,
        }
    }
}

impl crate::Journal for Counted {
    fn before_change(
        &mut self,
        id: &str,
        from: &str,
        to: &str,
        _: Option<&str>,
    ) -> Result<(), String> {
        if self.refuses {
            return Err("the journal is full".into());
        }
        self.lines.push((id.into(), from.into(), to.into()));
        self.refreshed_at_line
            .push(self.refreshed.load(Ordering::SeqCst));
        Ok(())
    }
}

impl Item for Placed {
    fn id(&self) -> String {
        self.id.into()
    }
    fn family(&self) -> &'static str {
        self.family
    }
    fn layer(&self) -> Layer {
        self.layer
    }
    fn class(&self) -> Class {
        self.class
    }
    fn requires(&self) -> Requires {
        self.requires
    }
    fn title(&self) -> String {
        self.id.into()
    }
    fn probe(&self) -> Probe {
        self.probed.fetch_add(1, Ordering::SeqCst);
        let mut probes = self.probes.lock().unwrap();
        if probes.len() > 1 {
            probes.remove(0)
        } else {
            probes[0].clone()
        }
    }
    fn apply(&self) -> Result<(), String> {
        self.applied.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn refresh(&self) -> Option<Result<(), String>> {
        self.refresh.map(|_| {
            self.refreshed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn refresh_changes(&self) -> bool {
        self.refresh == Some(true)
    }
    fn explain(&self, probe: &Probe) -> Explain {
        explained(probe)
    }
    fn after(&self) -> Vec<String> {
        self.after.clone()
    }
}

#[test]
fn outside_the_accounts_own_shell_every_item_is_read_and_no_change_is_made() {
    for (own, mode) in [
        (false, Mode::Apply),
        (false, Mode::Plan),
        (true, Mode::Apply),
    ] {
        let would = Placed::new("base.would", Class::Relay, Requires::Account);
        let right = Placed {
            probes: Mutex::new(vec![ok()]),
            refresh: Some(true),
            ..Placed::new("base.right", Class::Relay, Requires::Account)
        };
        let console = Placed {
            layer: Layer::Workstation,
            ..Placed::new("workstation.y", Class::Preference, Requires::Console)
        };
        let (would_applied, refreshed, console_probed) = (
            Arc::clone(&would.applied),
            Arc::clone(&right.refreshed),
            Arc::clone(&console.probed),
        );
        let items: Vec<Box<dyn Item>> = vec![Box::new(would), Box::new(right), Box::new(console)];
        let scope = Scope {
            not_own_account: (!own).then_some("it runs as root"),
            ..Scope::EVERY
        };
        let mut journal = Counted::new(&refreshed);
        let rows = run_scoped(&items, mode, scope, &mut journal, &mut |_| {}).unwrap();
        if own {
            assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
            assert_eq!(rows[1].outcome, Outcome::Ok, "{rows:?}");
            assert_eq!(rows[2].outcome, Outcome::Changed, "{rows:?}");
            assert_eq!(would_applied.load(Ordering::SeqCst), 1);
            assert_eq!(refreshed.load(Ordering::SeqCst), 1);
            assert_eq!(journal.lines.len(), 3, "{:?}", journal.lines);
            continue;
        }
        let held = not_own_account("it runs as root");
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{mode:?} {rows:?}");
        assert_eq!(rows[0].why.as_deref(), Some(held.as_str()));
        assert!(rows[0].from.is_some(), "the change is named: {rows:?}");
        assert_eq!(rows[0].stood_aside, None, "no retry clears it");
        assert!(rows[0].explain.is_some());
        assert_eq!(rows[1].outcome, Outcome::Ok, "{rows:?}");
        assert_eq!(rows[2].outcome, Outcome::Blocked, "{rows:?}");
        assert_eq!(rows[2].why.as_deref(), Some(held.as_str()));
        assert_eq!(would_applied.load(Ordering::SeqCst), 0, "nothing applied");
        assert_eq!(refreshed.load(Ordering::SeqCst), 0, "nothing refreshed");
        assert_eq!(console_probed.load(Ordering::SeqCst), 0, "not read");
        assert!(journal.lines.is_empty(), "nothing journaled");
    }
}

#[test]
fn a_refresh_that_changes_the_machine_is_journaled_before_it_runs() {
    for (changes, refuses) in [(true, false), (true, true), (false, false)] {
        let item = Placed {
            probes: Mutex::new(vec![ok()]),
            refresh: Some(changes),
            ..Placed::new("base.pkg.shims", Class::Relay, Requires::Account)
        };
        let refreshed = Arc::clone(&item.refreshed);
        let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
        let mut journal = Counted {
            refuses,
            ..Counted::new(&refreshed)
        };
        let rows = run_scoped(
            &items,
            Mode::Apply,
            Scope::REPAIR,
            &mut journal,
            &mut |_| {},
        )
        .unwrap();
        match (changes, refuses) {
            (true, false) => {
                assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
                assert_eq!(
                    journal.lines,
                    [("base.pkg.shims".into(), "b".into(), "b".into())]
                );
                assert_eq!(journal.refreshed_at_line, [0], "the line came first");
                assert_eq!(refreshed.load(Ordering::SeqCst), 1);
            }
            (true, true) => {
                assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
                assert!(
                    rows[0]
                        .why
                        .as_deref()
                        .is_some_and(|why| why.contains("the journal refused the record")),
                    "{rows:?}"
                );
                assert_eq!(refreshed.load(Ordering::SeqCst), 0, "left unrun");
            }
            _ => {
                assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
                assert!(journal.lines.is_empty(), "an activation writes no line");
                assert_eq!(refreshed.load(Ordering::SeqCst), 1);
            }
        }
    }
}

#[test]
fn a_held_back_item_is_read_and_blocked_and_never_applied() {
    let would = Placed::new("base.pkg.shims", Class::Relay, Requires::Account);
    let applied = Arc::clone(&would.applied);
    let right = Placed {
        probes: Mutex::new(vec![ok()]),
        ..Placed::new("base.shell.hooks", Class::Relay, Requires::Account)
    };
    let items = held_back(
        vec![Box::new(would), Box::new(right)],
        "the store lock could not be taken (a symlink)",
    );
    let refreshed = Arc::new(AtomicU32::new(0));
    let mut journal = Counted::new(&refreshed);
    let rows = crate::run(&items, Mode::Apply, &mut journal).unwrap();
    assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
    assert_eq!(
        rows[0].why.as_deref(),
        Some("a; not changed: the store lock could not be taken (a symlink)"),
        "what it would have changed, and why not"
    );
    assert_eq!(rows[0].stood_aside, None);
    assert_eq!(rows[1].outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(applied.load(Ordering::SeqCst), 0);
    assert!(journal.lines.is_empty());
}

#[test]
fn an_item_put_off_is_read_and_deferred_and_never_applied() {
    let would = Placed::new("workstation.dock", Class::Preference, Requires::Account);
    let applied = Arc::clone(&would.applied);
    let right = Placed {
        probes: Mutex::new(vec![ok()]),
        ..Placed::new("workstation.finder", Class::Preference, Requires::Account)
    };
    let items = put_off(
        vec![Box::new(would), Box::new(right)],
        "its last try did not take; it is tried again later",
    );
    let refreshed = Arc::new(AtomicU32::new(0));
    let mut journal = Counted::new(&refreshed);
    let rows = crate::run(&items, Mode::Apply, &mut journal).unwrap();
    assert_eq!(rows[0].outcome, Outcome::Deferred, "{rows:?}");
    assert_eq!(
        rows[0].why.as_deref(),
        Some("a; not changed yet: its last try did not take; it is tried again later"),
        "what it would have changed, and why not now"
    );
    assert_eq!(rows[0].stood_aside, None);
    assert_eq!(rows[1].outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(applied.load(Ordering::SeqCst), 0);
    assert!(journal.lines.is_empty());
}

#[test]
fn an_item_left_as_is_is_read_and_not_applicable_and_never_applied() {
    let would = Placed::new("workstation.widgets", Class::Preference, Requires::Account);
    let applied = Arc::clone(&would.applied);
    let right = Placed {
        probes: Mutex::new(vec![ok()]),
        refresh: Some(false),
        ..Placed::new("workstation.dock", Class::Preference, Requires::Account)
    };
    let refreshed = Arc::clone(&right.refreshed);
    let items = left_as_is(
        vec![Box::new(would), Box::new(right)],
        "set once and changed since",
    );
    let mut journal = Counted::new(&refreshed);
    let rows = crate::run(&items, Mode::Apply, &mut journal).unwrap();
    assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
    assert_eq!(
        rows[0].why.as_deref(),
        Some("a; left as it is: set once and changed since"),
        "what it reads now, and why it stays"
    );
    assert_eq!(rows[0].stood_aside, None);
    assert_eq!(rows[1].outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(applied.load(Ordering::SeqCst), 0);
    assert_eq!(refreshed.load(Ordering::SeqCst), 0, "nor refreshed");
    assert!(journal.lines.is_empty());
}

#[test]
fn away_from_the_console_an_account_item_applies_and_a_console_item_is_blocked() {
    for at_console in [false, true] {
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Placed::new("base.x", Class::Relay, Requires::Account)),
            Box::new(Placed {
                layer: Layer::Workstation,
                ..Placed::new("workstation.y", Class::Preference, Requires::Console)
            }),
        ];
        let scope = Scope {
            at_console,
            ..Scope::EVERY
        };
        let rows = run_scoped(&items, Mode::Apply, scope, &mut NoJournal, &mut |_| {}).unwrap();
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        if at_console {
            assert_eq!(rows[1].outcome, Outcome::Changed, "{rows:?}");
        } else {
            assert_eq!(rows[1].outcome, Outcome::Blocked, "{rows:?}");
            assert_eq!(rows[1].why.as_deref(), Some(NOT_AT_CONSOLE));
            assert!(rows[1].explain.is_some(), "{rows:?}");
        }
    }
    let console = Placed::new("workstation.y", Class::Preference, Requires::Console);
    let probed = Arc::clone(&console.probed);
    let items: Vec<Box<dyn Item>> = vec![Box::new(console)];
    let away = Scope {
        at_console: false,
        ..Scope::EVERY
    };
    let rows = run_scoped(&items, Mode::Plan, away, &mut NoJournal, &mut |_| {}).unwrap();
    assert_eq!(rows[0].outcome, Outcome::Blocked);
    assert_eq!(probed.load(Ordering::SeqCst), 0);
}

#[test]
fn an_apply_run_plans_the_classes_its_surface_does_not_apply() {
    let relay = Placed::new("base.relay", Class::Relay, Requires::Account);
    let asked = Placed::new("base.asked", Class::Asked, Requires::Account);
    let preference = Placed::new("workstation.pref", Class::Preference, Requires::Account);
    let (relay_applied, preference_applied) =
        (Arc::clone(&relay.applied), Arc::clone(&preference.applied));
    let items: Vec<Box<dyn Item>> = vec![Box::new(relay), Box::new(asked), Box::new(preference)];
    let repair = Scope::REPAIR;
    let rows = run_scoped(&items, Mode::Apply, repair, &mut NoJournal, &mut |_| {}).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.outcome).collect::<Vec<_>>(),
        [Outcome::Changed, Outcome::Changed, Outcome::Planned]
    );
    assert_eq!(preference_applied.load(Ordering::SeqCst), 0);
    assert_eq!(relay_applied.load(Ordering::SeqCst), 1);
    let relay_only = Scope {
        applies: &[Class::Relay],
        ..Scope::REPAIR
    };
    let asked = Placed::new("base.asked", Class::Asked, Requires::Account);
    let asked_applied = Arc::clone(&asked.applied);
    let items: Vec<Box<dyn Item>> = vec![Box::new(asked)];
    let rows = run_scoped(&items, Mode::Apply, relay_only, &mut NoJournal, &mut |_| {}).unwrap();
    assert_eq!(rows[0].outcome, Outcome::Planned);
    assert_eq!(asked_applied.load(Ordering::SeqCst), 0);
}

#[test]
fn a_dependant_of_a_planned_item_waits_for_it() {
    let pref = Placed::new("workstation.pref", Class::Preference, Requires::Account);
    let relay = Placed {
        after: vec!["workstation.pref".into()],
        ..Placed::new("base.relay", Class::Relay, Requires::Account)
    };
    let relay_applied = Arc::clone(&relay.applied);
    let items: Vec<Box<dyn Item>> = vec![Box::new(pref), Box::new(relay)];
    let repair = Scope::REPAIR;
    let rows = run_scoped(&items, Mode::Apply, repair, &mut NoJournal, &mut |_| {}).unwrap();
    assert_eq!(rows[1].outcome, Outcome::Blocked, "{rows:?}");
    assert_eq!(relay_applied.load(Ordering::SeqCst), 0);
}

#[test]
fn a_token_names_a_family_a_layer_an_id_or_an_id_it_prefixes() {
    let base = Some(Layer::Base);
    assert!(selects("pkg", "base.pkg.shims", "pkg", base));
    assert!(selects("base", "base.pkg.shims", "pkg", base));
    assert!(selects("base.pkg.shims", "base.pkg.shims", "pkg", base));
    assert!(selects("base.pkg", "base.pkg.shims", "pkg", base));
    assert!(selects(
        "base.shell.rc",
        "base.shell.rc@.zshrc",
        "shell",
        base
    ));
    assert!(!selects("base.pkg.shim", "base.pkg.shims", "pkg", base));
    assert!(!selects("shell", "base.pkg.shims", "pkg", base));
    assert!(!selects("workstation", "base.pkg.shims", "pkg", base));
    assert!(!selects("", "base.pkg.shims", "pkg", base));
    assert!(selects("develop", "develop.checkout@~/aterm", "git", None));
    assert!(!selects("workstation", "base.pkg.shims", "pkg", None));
    let develop = Some(Layer::Develop);
    let instance = "develop.checkout@~/aterm";
    assert!(selects(instance, instance, "checkout", develop));
    assert!(!selects(
        instance,
        "develop.checkout@~/aterm.old",
        "checkout",
        develop
    ));
    assert!(!selects(
        instance,
        "develop.checkout@~/aterm@2",
        "checkout",
        develop
    ));
    assert!(selects(
        "develop.checkout",
        "develop.checkout@~/aterm.old",
        "checkout",
        develop
    ));
}

#[test]
fn select_keeps_what_it_names_and_narrows_edges_to_the_run() {
    let make = || -> Vec<Box<dyn Item>> {
        vec![
            Box::new(Placed::new(
                "base.shell.hooks",
                Class::Relay,
                Requires::Account,
            )),
            Box::new(Placed {
                after: vec!["base.shell.hooks".into()],
                ..Placed::new("base.shell.rc", Class::Relay, Requires::Account)
            }),
            Box::new(Placed {
                family: "pkg",
                ..Placed::new("base.pkg.shims", Class::Relay, Requires::Account)
            }),
        ]
    };
    let (kept, unmatched) = select(make(), &[]);
    assert_eq!(kept.len(), 3);
    assert!(unmatched.is_empty());
    assert_eq!(kept[1].after(), ["base.shell.hooks"]);

    let (kept, unmatched) = select(make(), &["base.shell.rc".into(), "nosuch".into()]);
    assert_eq!(
        kept.iter().map(|i| i.id()).collect::<Vec<_>>(),
        ["base.shell.rc"]
    );
    assert!(
        kept[0].after().is_empty(),
        "the left-out hooks item is no edge"
    );
    assert_eq!(unmatched, ["nosuch"]);
    let rows = crate::run(&kept, Mode::Plan, &mut NoJournal).unwrap();
    assert_eq!(rows[0].outcome, Outcome::Planned);

    let (kept, _) = select(make(), &["base.shell".into()]);
    assert_eq!(
        kept[1].after(),
        ["base.shell.hooks"],
        "both kept: the edge stays"
    );
    let (kept, _) = select(make(), &["pkg".into()]);
    assert_eq!(
        kept.iter().map(|i| i.id()).collect::<Vec<_>>(),
        ["base.pkg.shims"]
    );
}

#[test]
fn a_standing_aside_item_is_probed_never_applied_and_blocked_on_the_lock() {
    let would = Placed {
        probes: Mutex::new(vec![change()]),
        ..Placed::new("base.pkg.shims", Class::Relay, Requires::Account)
    };
    let (would_probed, would_applied) = (Arc::clone(&would.probed), Arc::clone(&would.applied));
    let done = Placed {
        probes: Mutex::new(vec![ok()]),
        ..Placed::new("base.shell.hooks", Class::Relay, Requires::Account)
    };
    let waits = Placed {
        after: vec!["base.pkg.shims".into()],
        ..Placed::new("base.pkg.exec-roots", Class::Relay, Requires::Account)
    };
    let waits_probed = Arc::clone(&waits.probed);
    let lock = StoodAside::Lock("an install pass holds the store".into());
    let items = standing_aside(
        vec![Box::new(would), Box::new(done), Box::new(waits)],
        &lock,
    );
    let mut journal = Counted::new(&Arc::new(AtomicU32::new(0)));
    let rows = crate::run(&items, Mode::Apply, &mut journal).unwrap();
    assert!(
        journal.lines.is_empty(),
        "no journal line for a change nobody makes"
    );
    assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
    assert_eq!(rows[0].stood_aside, Some(lock.clone()));
    assert_eq!(
        rows[0].stood_aside.as_ref().map(StoodAside::exit_status),
        Some(75)
    );
    assert_eq!(rows[1].outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(rows[2].outcome, Outcome::Blocked, "{rows:?}");
    assert_eq!(rows[2].stood_aside, Some(lock.clone()), "{rows:?}");
    assert_eq!(
        waits_probed.load(Ordering::SeqCst),
        0,
        "a waiting item is not read"
    );
    assert_eq!(would_probed.load(Ordering::SeqCst), 1);
    assert_eq!(would_applied.load(Ordering::SeqCst), 0, "nothing applied");
    let (kept, _) = select(items, &["base.pkg.shims".into()]);
    let rows = crate::run(&kept, Mode::Apply, &mut journal).unwrap();
    assert_eq!(rows[0].stood_aside, Some(lock), "{rows:?}");
    assert!(
        journal.lines.is_empty(),
        "no journal line for a change nobody makes"
    );
    assert_eq!(would_applied.load(Ordering::SeqCst), 0, "nothing applied");
}

/// An item ANOTHER RUN LAYS (an install pass holding the store) reads `NotApplicable` and
/// waits on no other item: one after a predecessor left the same way is read, not blocked,
/// and nothing is applied or journaled. NEGATIVE CONTROL: [`left_as_is`] keeps the item's
/// `after`, so the same successor is blocked on the predecessor it waits for.
#[test]
fn an_item_another_run_lays_waits_on_nothing() {
    for another in [true, false] {
        let first = Placed::new("base.shell.hooks", Class::Relay, Requires::Account);
        let second = Placed {
            after: vec!["base.shell.hooks".to_owned()],
            ..Placed::new("base.shell.rc", Class::Relay, Requires::Account)
        };
        // An item whose own probe waits on the first, as atpkg's rc item does.
        let third = Placed {
            probes: Mutex::new(vec![Probe::Waits {
                on: "base.shell.hooks".to_owned(),
                why: "the hooks are not current".to_owned(),
            }]),
            ..Placed::new("base.shell.rc-probe", Class::Relay, Requires::Account)
        };
        let applied = [
            Arc::clone(&first.applied),
            Arc::clone(&second.applied),
            Arc::clone(&third.applied),
        ];
        let items: Vec<Box<dyn Item>> = vec![Box::new(first), Box::new(second), Box::new(third)];
        let why = "the running install pass lays these";
        let items = if another {
            left_to_another(items, why)
        } else {
            left_as_is(items, why)
        };
        let refreshed = Arc::new(AtomicU32::new(0));
        let mut journal = Counted::new(&refreshed);
        let rows = run_scoped(
            &items,
            Mode::Apply,
            Scope::REPAIR,
            &mut journal,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
        if another {
            for row in &rows[1..] {
                assert_eq!(row.outcome, Outcome::NotApplicable, "{rows:?}");
                assert!(
                    row.why
                        .as_deref()
                        .is_some_and(|said| said.ends_with(&format!("left as it is: {why}"))),
                    "{rows:?}"
                );
            }
        } else {
            assert_eq!(rows[1].outcome, Outcome::Blocked, "{rows:?}");
            assert_eq!(rows[1].why.as_deref(), Some("waits for base.shell.hooks"));
            assert_eq!(rows[2].outcome, Outcome::Blocked, "{rows:?}");
            assert_eq!(rows[2].why.as_deref(), Some("the hooks are not current"));
        }
        assert!(applied.iter().all(|n| n.load(Ordering::SeqCst) == 0));
        assert!(journal.lines.is_empty(), "{:?}", journal.lines);
    }

    struct DummyAct;
    impl crate::Activation for DummyAct {
        fn id(&self) -> String {
            "act.dummy".into()
        }
        fn pending(&self) -> Result<Option<(String, String)>, String> {
            Ok(None)
        }
        fn activate(&self) -> Result<Vec<(String, String)>, String> {
            Ok(Vec::new())
        }
    }
    struct WithAct(Placed, Arc<DummyAct>);
    impl Item for WithAct {
        fn id(&self) -> String {
            self.0.id()
        }
        fn family(&self) -> &'static str {
            self.0.family()
        }
        fn layer(&self) -> Layer {
            self.0.layer()
        }
        fn class(&self) -> Class {
            self.0.class()
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
            self.0.explain(probe)
        }
        fn activation(&self) -> Option<Arc<dyn crate::Activation + '_>> {
            Some(Arc::clone(&self.1) as Arc<dyn crate::Activation>)
        }
    }
    let act = Arc::new(DummyAct);
    let mk = || -> Vec<Box<dyn Item>> {
        vec![Box::new(WithAct(
            Placed::new("base.act", Class::Relay, Requires::Account),
            Arc::clone(&act),
        ))]
    };
    let aside = standing_aside(mk(), &StoodAside::Lock("lock".into()));
    assert_eq!(
        aside[0].activation().map(|a| a.id()),
        Some("act.dummy".into())
    );
    let hb = held_back(mk(), "why");
    assert_eq!(hb[0].activation().map(|a| a.id()), Some("act.dummy".into()));
    let later = put_off(mk(), "why");
    assert_eq!(
        later[0].activation().map(|a| a.id()),
        Some("act.dummy".into())
    );
    let lai = left_as_is(mk(), "why");
    assert_eq!(
        lai[0].activation().map(|a| a.id()),
        Some("act.dummy".into())
    );
    let lta = left_to_another(mk(), "why");
    assert!(lta[0].activation().is_none());
}

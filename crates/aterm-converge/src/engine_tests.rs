// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machines engine's tests, moved with the engine (design §10 S3: the regression
//! proof).
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

#[test]
fn a_successful_read_back_reports_the_current_value_instead_of_the_old_target() {
    for now in ["b", "left off by the owner"] {
        let (row, applied) = one(
            Scripted::new(vec![change(), Probe::Ok { now: now.into() }], Ok(())),
            Mode::Apply,
        );
        assert_eq!(applied, 1);
        assert_eq!(row.outcome, Outcome::Changed);
        assert_eq!(row.from.as_deref(), Some("a"));
        assert_eq!(row.to.as_deref(), Some(now));
    }
    let (planned, applied) = one(Scripted::new(vec![change()], Ok(())), Mode::Plan);
    assert_eq!(applied, 0);
    assert_eq!(
        planned.to.as_deref(),
        Some("b"),
        "a plan retains its target"
    );
}

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
        Box::new(Named {
            id: "github.after-urls",
            probe: waits("github.ssh-urls"),
            after: vec!["github.ssh-urls".into()],
        }),
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
        Box::new(Named {
            id: "github.git",
            probe: Probe::Unusable {
                why: "~/.gitconfig: Permission denied".into(),
            },
            after: vec!["developer.clt".into()],
        }),
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
    fn before_change(&mut self, _: &str, _: &str, _: &str, _: Option<&str>) -> Result<(), String> {
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
    assert_eq!(rows[1].outcome, Outcome::Unknown);
    assert_eq!(rows[3].outcome, Outcome::Blocked);
    assert_eq!(rows[3].why.as_deref(), Some("waits for base.pkg.toolset"));
    assert_eq!(rows[4].outcome, Outcome::Blocked);
    assert_eq!(order(&items[2..]).unwrap(), [0, 1, 2]);
}

#[test]
fn a_cycle_is_refused_before_anything_is_probed() {
    let probes = Arc::new(AtomicU32::new(0));
    let items = vec![
        counted("a", change(), &["c"], &probes),
        counted("b", ok(), &[], &probes),
        counted("c", change(), &["a"], &probes),
        counted("d", ok(), &["c"], &probes),
    ];
    assert_eq!(
        run(&items, Mode::Apply, &mut NoJournal).unwrap_err(),
        Refusal::Cycle {
            ids: vec!["a".into(), "c".into(), "d".into()]
        }
    );
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
    assert_eq!(probes.load(Ordering::SeqCst), 2);
}

struct Aside(Scripted, StoodAside);

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
        assert_eq!(
            rows[0].explain,
            Some(Explain {
                why: aside.why().into(),
                remedy,
            })
        );
    }
}

/// An item whose refresh stands aside, as [`Aside`]'s apply does.
struct RefreshAside(Scripted, StoodAside);

impl Item for RefreshAside {
    fn id(&self) -> String {
        self.0.id()
    }
    fn family(&self) -> &'static str {
        "refresh-test"
    }
    fn layer(&self) -> Layer {
        Layer::Develop
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
    fn refresh(&self) -> Option<Result<(), String>> {
        Some(Ok(()))
    }
    fn explain(&self, probe: &Probe) -> Explain {
        explained(probe)
    }
    fn stood_aside(&self) -> Option<StoodAside> {
        Some(self.1.clone())
    }
}

/// A refresh that stood aside did not run: its row is blocked with the kind it stood
/// aside for, as a stood-aside apply's is, and its read-back is not taken (here it would
/// read a change, which would fail the row).
#[test]
fn a_stood_aside_refresh_is_blocked_with_its_kind_and_is_not_read_back() {
    let aside = StoodAside::Waiting("you stopped setup in System State".into());
    let item = Scripted::new(vec![ok(), change()], Ok(()));
    let probed = Arc::clone(&item.probed);
    let items: Vec<Box<dyn Item>> = vec![Box::new(RefreshAside(item, aside.clone()))];
    let rows = run(&items, Mode::Apply, &mut NoJournal).unwrap();
    assert_eq!(rows[0].outcome, Outcome::Blocked, "{:?}", rows[0]);
    assert_eq!(rows[0].why.as_deref(), Some(aside.why()));
    assert_eq!(rows[0].stood_aside.as_ref(), Some(&aside));
    assert_eq!(
        probed.load(Ordering::SeqCst),
        1,
        "the refresh was read back"
    );
    assert_eq!(
        rows[0].explain.as_ref().map(|e| e.why.as_str()),
        Some(aside.why())
    );
}

struct ReadAside(Scripted, Option<StoodAside>);

impl Item for ReadAside {
    fn id(&self) -> String {
        self.0.id()
    }
    fn family(&self) -> &'static str {
        "git"
    }
    fn layer(&self) -> Layer {
        Layer::Develop
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
        explained(probe)
    }
    fn read_aside(&self) -> Option<StoodAside> {
        self.1.clone()
    }
}

#[test]
fn an_unreadable_probe_that_lacked_the_network_stands_aside_offline() {
    let offline = StoodAside::Offline("origin could not be reached".into());
    let unusable = || Probe::Unusable {
        why: "cannot read origin's default branch".into(),
    };
    for mode in [Mode::Plan, Mode::Apply] {
        let item = Scripted::new(vec![unusable()], Ok(()));
        let applies = Arc::clone(&item.applies);
        let waiting = Named {
            id: "develop.tools",
            probe: ok(),
            after: vec![item.id()],
        };
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(ReadAside(item, Some(offline.clone()))),
            Box::new(waiting),
        ];
        let rows = run(&items, mode, &mut NoJournal).unwrap();
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{mode:?}");
        assert_eq!(rows[0].why.as_deref(), Some(offline.why()));
        assert_eq!(rows[0].stood_aside, Some(offline.clone()));
        assert_eq!(
            rows[0].explain.as_ref().map(|explain| &explain.remedy),
            Some(&Remedy::Network)
        );
        assert_eq!(applies.load(Ordering::SeqCst), 0);
        if mode == Mode::Apply {
            assert_eq!(rows[1].outcome, Outcome::Blocked);
            assert_eq!(rows[1].stood_aside, Some(offline.clone()));
        }
    }
    let control = ReadAside(Scripted::new(vec![unusable()], Ok(())), None);
    let items: Vec<Box<dyn Item>> = vec![Box::new(control)];
    let rows = run(&items, Mode::Apply, &mut NoJournal).unwrap();
    assert_eq!(rows[0].outcome, Outcome::Unknown);
    assert_eq!(
        rows[0].why.as_deref(),
        Some("cannot read origin's default branch")
    );
    assert_eq!(rows[0].stood_aside, None);
}

/// A row that did not run because what it waits for stood aside carries that stand-aside and
/// names the row it comes from: the one that stood aside of its own accord, through every
/// row behind it, so a caller tells an inherited wait (and its source) from an item's own.
/// NEGATIVE CONTROL: the row that stood aside itself, and a row behind a prerequisite that
/// failed without standing aside, name none.
#[test]
fn an_inherited_stand_aside_names_the_row_it_comes_from() {
    let waiting = StoodAside::Waiting("an admin approves this Mac".into());
    let unusable = || Probe::Unusable {
        why: "the vault has not let this Mac in".into(),
    };
    let vault = Scripted::new(vec![unusable()], Ok(()));
    let origin = vault.id();
    let auth = Named {
        id: "test.auth",
        probe: ok(),
        after: vec![origin.clone()],
    };
    let repos = Named {
        id: "test.repos",
        probe: ok(),
        after: vec!["test.auth".into()],
    };
    let items: Vec<Box<dyn Item>> = vec![
        Box::new(ReadAside(vault, Some(waiting.clone()))),
        Box::new(auth),
        Box::new(repos),
    ];
    let rows = run(&items, Mode::Apply, &mut NoJournal).unwrap();
    assert_eq!(rows[0].stood_aside, Some(waiting.clone()));
    assert_eq!(rows[0].waits_on, None, "CONTROL: its own stand-aside");
    for row in &rows[1..] {
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert_eq!(row.stood_aside, Some(waiting.clone()), "{row:?}");
        assert_eq!(row.waits_on.as_deref(), Some(origin.as_str()), "{row:?}");
    }
    let failed = Scripted::new(vec![unusable()], Ok(()));
    let behind = Named {
        id: "test.behind",
        probe: ok(),
        after: vec![failed.id()],
    };
    let items: Vec<Box<dyn Item>> = vec![Box::new(ReadAside(failed, None)), Box::new(behind)];
    let rows = run(&items, Mode::Apply, &mut NoJournal).unwrap();
    assert_eq!(rows[1].outcome, Outcome::Blocked);
    assert_eq!(
        (&rows[1].stood_aside, &rows[1].waits_on),
        (&None, &None),
        "CONTROL: {:?}",
        rows[1]
    );
}

#[test]
fn a_failed_refresh_and_a_refused_record_are_explained_by_what_failed() {
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

type Events = Arc<std::sync::Mutex<Vec<String>>>;

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

struct Writer {
    id: &'static str,
    class: Class,
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
            class: Class::Preference,
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
        self.class
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

struct Recorded {
    events: Events,
    refuses: &'static [&'static str],
}

impl Journal for Recorded {
    fn before_change(&mut self, id: &str, _: &str, _: &str, _: Option<&str>) -> Result<(), String> {
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
        let mut unchanged = Writer::new("c", &events, Some(&dock));
        unchanged.probes = std::sync::Mutex::new(vec![ok(), ok()]);
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Writer::new("a", &events, Some(&dock))),
            Box::new(Writer::new("b", &events, Some(&dock))),
            Box::new(unchanged),
        ];
        let rows = recorded(&items, Mode::Apply, &events, refuses);
        for row in &rows[..2] {
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
        assert_eq!(rows[2].outcome, Outcome::Ok);
        assert!(rows[2].from.is_none() && rows[2].revert.is_none());
        let events = events.lock().unwrap();
        assert_eq!(
            events.iter().any(|event| event == "activate relaunch.dock"),
            ran,
            "{events:?}"
        );
        assert_eq!(events.iter().filter(|event| *event == "probe a").count(), 1);
        assert_eq!(
            events.iter().filter(|event| *event == "probe c").count(),
            if ran { 2 } else { 1 }
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.as_str(), "apply c" | "journal c"))
        );
        assert_eq!(events.iter().filter(|event| *event == "row c").count(), 1);
    }
}

#[test]
fn a_shared_activation_rechecks_initially_correct_items_without_reapplying_them() {
    for after in [
        ok(),
        change(),
        Probe::Unusable {
            why: "the reader cannot be read".into(),
        },
    ] {
        for result in [Ok(Vec::new()), Err("partially relaunched".into())] {
            let events = Events::default();
            let dock = Arc::new(Relaunch {
                id: "relaunch.dock",
                events: Arc::clone(&events),
                pending: Ok(Some(("old".into(), "new".into()))),
                result,
            });
            let mut unchanged = Writer::new("unchanged", &events, Some(&dock));
            unchanged.probes = std::sync::Mutex::new(vec![ok(), after.clone()]);
            let mut outside = Writer::new("outside", &events, None);
            outside.probes = std::sync::Mutex::new(vec![ok(), change()]);
            let items: Vec<Box<dyn Item>> = vec![
                Box::new(unchanged),
                Box::new(outside),
                Box::new(Writer::new("writer", &events, Some(&dock))),
            ];
            let rows = recorded(&items, Mode::Apply, &events, &[]);
            let row = &rows[0];
            assert_eq!(
                row.outcome,
                if matches!(after, Probe::Ok { .. }) {
                    Outcome::Ok
                } else {
                    Outcome::Failed
                },
                "{rows:?}"
            );
            assert!(row.from.is_none() && row.revert.is_none());
            assert_eq!(rows[1].outcome, Outcome::Ok);
            if row.outcome == Outcome::Failed {
                assert_eq!(row.explain, Some(explained(&after)));
                assert!(row.why.as_ref().is_some_and(|why| !why.is_empty()));
            }
            let events = events.lock().unwrap();
            let at = |event: &str| events.iter().position(|seen| seen == event).unwrap();
            assert!(at("activate relaunch.dock") < at("row unchanged"));
            assert_eq!(
                events
                    .iter()
                    .filter(|event| *event == "probe unchanged")
                    .count(),
                2
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| *event == "probe outside")
                    .count(),
                1
            );
            for id in ["unchanged", "outside", "writer"] {
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| **event == format!("row {id}"))
                        .count(),
                    1
                );
            }
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event.as_str(), "apply unchanged" | "journal unchanged"))
            );
        }
    }
}

#[test]
fn a_class_read_as_a_plan_is_not_rechecked_by_an_applied_shared_activation() {
    for mode in [Mode::Plan, Mode::Apply] {
        let events = Events::default();
        let dock = relaunch("relaunch.dock", &events);
        let mut unchanged = Writer::new("unchanged", &events, Some(&dock));
        unchanged.probes = std::sync::Mutex::new(vec![ok(), change()]);
        let mut writer = Writer::new("writer", &events, Some(&dock));
        writer.class = Class::Relay;
        let items: Vec<Box<dyn Item>> = vec![Box::new(unchanged), Box::new(writer)];
        let mut journal = Recorded {
            events: Arc::clone(&events),
            refuses: &[],
        };
        let rows = run_scoped(&items, mode, Scope::REPAIR, &mut journal, &mut |_| {}).unwrap();
        assert_eq!(rows[0].outcome, Outcome::Ok);
        assert_eq!(
            rows[1].outcome,
            if mode == Mode::Apply {
                Outcome::Changed
            } else {
                Outcome::Planned
            }
        );
        let events = events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| *event == "probe unchanged")
                .count(),
            1
        );
        assert_eq!(
            events.iter().any(|event| event == "activate relaunch.dock"),
            mode == Mode::Apply
        );
    }
}

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
    assert_eq!(events.iter().filter(|event| *event == "probe a").count(), 1);
    assert_eq!(events.iter().filter(|event| *event == "probe b").count(), 2);
}

#[test]
fn an_activation_runs_only_when_an_applied_item_awaits_it_and_something_is_pending() {
    let events = Events::default();
    let dock = Arc::new(Relaunch {
        id: "relaunch.dock",
        events: Arc::clone(&events),
        pending: Ok(None),
        result: Ok(Vec::new()),
    });
    let mut unchanged = Writer::new("unchanged", &events, Some(&dock));
    unchanged.probes = std::sync::Mutex::new(vec![ok(), change()]);
    let items: Vec<Box<dyn Item>> = vec![
        Box::new(Writer::new("a", &events, Some(&dock))),
        Box::new(unchanged),
    ];
    let rows = recorded(&items, Mode::Apply, &events, &[]);
    assert_eq!(rows[0].outcome, Outcome::Changed);
    assert_eq!(rows[1].outcome, Outcome::Ok);
    assert_eq!(
        *events.lock().unwrap(),
        [
            "probe a",
            "journal a",
            "apply a",
            "probe unchanged",
            "pending relaunch.dock",
            "probe a",
            "row a",
            "row unchanged",
        ]
    );
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
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| *event == "probe a")
            .count(),
        1
    );
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.contains("relaunch.dock")),
        "{:?}",
        events.lock()
    );
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

/// A workstation item: it runs only at the console, and is never probed elsewhere.
struct AtConsole;

impl Item for AtConsole {
    fn id(&self) -> String {
        "t.console".into()
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
    fn requires(&self) -> Requires {
        Requires::Console
    }
    fn title(&self) -> String {
        "at the console".into()
    }
    fn probe(&self) -> Probe {
        panic!("a console item away from the console is never probed")
    }
    fn apply(&self) -> Result<(), String> {
        Err("apply must not be reached".into())
    }
    fn explain(&self, probe: &Probe) -> Explain {
        explained(probe)
    }
}

/// EVERY ROW CARRIES THE TIME THE ENGINE SPENT ON IT, by the time `on_row` streams it:
/// done, changed, planned, blocked, stood aside after its apply and before it, waiting
/// on a prerequisite that stood aside, a console item away from the console (which
/// nothing probes), and the rows a shared activation completes after it runs (the
/// applied one and the one only rechecked). Presence only: what a row costs is the
/// host's, never a budget.
#[test]
fn every_row_carries_the_time_the_engine_spent_on_it() {
    let lock = StoodAside::Lock("another atpkg run held the store".into());
    let scripted = |id, probes| Scripted {
        id,
        ..Scripted::new(probes, Ok(()))
    };
    let events = Events::default();
    let dock = relaunch("relaunch.dock", &events);
    let items = || -> Vec<Box<dyn Item>> {
        let mut items: Vec<Box<dyn Item>> = vec![
            Box::new(scripted("t.ok", vec![ok()])),
            Box::new(scripted("t.changes", vec![change(), ok()])),
            Box::new(Named {
                id: "t.blocked",
                probe: Probe::Blocked {
                    why: "no disk".into(),
                },
                after: vec![],
            }),
            Box::new(Aside(
                scripted("t.aside", vec![change(), ok()]),
                lock.clone(),
            )),
            Box::new(Named {
                id: "t.waits",
                probe: ok(),
                after: vec!["t.aside".into()],
            }),
            Box::new(AtConsole),
            Box::new(Writer {
                probes: std::sync::Mutex::new(vec![ok()]),
                ..Writer::new("t.rechecked", &events, Some(&dock))
            }),
            Box::new(Writer::new("t.activated", &events, Some(&dock))),
        ];
        items.extend(crate::scope::standing_aside(
            vec![Box::new(scripted("t.held", vec![change()]))],
            &lock,
        ));
        items
    };
    let away = Scope {
        at_console: false,
        ..Scope::EVERY
    };
    for (mode, outcomes) in [
        (
            Mode::Apply,
            [
                Outcome::Ok,
                Outcome::Changed,
                Outcome::Blocked,
                Outcome::Blocked,
                Outcome::Blocked,
                Outcome::Blocked,
                Outcome::Ok,
                Outcome::Changed,
                Outcome::Blocked,
            ],
        ),
        (
            Mode::Plan,
            [
                Outcome::Ok,
                Outcome::Planned,
                Outcome::Blocked,
                Outcome::Planned,
                Outcome::Ok,
                Outcome::Blocked,
                Outcome::Ok,
                Outcome::Planned,
                Outcome::Planned,
            ],
        ),
    ] {
        let mut streamed = Vec::new();
        let rows = run_scoped(&items(), mode, away, &mut NoJournal, &mut |row| {
            streamed.push((row.id.clone(), row.elapsed_ms));
        })
        .unwrap();
        assert_eq!(
            rows.iter().map(|r| r.outcome).collect::<Vec<_>>(),
            outcomes,
            "{mode:?}"
        );
        if mode == Mode::Apply {
            // The two kinds of standing aside, the row waiting on one, and the shared
            // activation that ran.
            assert!(rows[3].stood_aside.is_some() && rows[4].stood_aside.is_some());
            assert!(rows[8].stood_aside.is_some());
            assert!(
                events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|event| event == "activate relaunch.dock")
            );
        }
        assert!(
            rows.iter().all(|r| r.elapsed_ms.is_some()),
            "{mode:?}: {rows:?}"
        );
        streamed.sort();
        let mut expected: Vec<_> = rows.iter().map(|r| (r.id.clone(), r.elapsed_ms)).collect();
        expected.sort();
        assert_eq!(streamed, expected, "{mode:?}");
    }
}

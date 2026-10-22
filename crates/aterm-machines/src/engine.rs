// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The engine (design §5) is `aterm_converge`'s (docs/DESIGN-init-system-2026-10-20.md
//! §3): this module binds machines items, which take their `System` and context per call,
//! to it ([`Bound`]), so the sixteen families keep their code.

use crate::items::{Activation, Ctx, Item};
use crate::model::{ItemReport, Mode, Probe};
use crate::port::System;
use aterm_converge::{Class, Effect, Explain, Layer, Remedy, Requires, StoodAside};
use std::sync::Arc;

#[cfg(any(test, debug_assertions))]
pub use aterm_converge::NoJournal;
pub use aterm_converge::{Journal, Refusal};

/// Where a machines item sits in the desired state. Its family does not say: the
/// `keyboard` family is a workstation preference under `aterm init` and a fleet setting
/// under `aterm machines`, so the surface that composes the run places it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// The layer.
    pub layer: Layer,
    /// What its apply does.
    pub class: Class,
    /// Where it may run.
    pub requires: Requires,
    /// The act that makes a change of this run's: the surface's own command.
    pub remedy: Remedy,
}

impl Place {
    /// A fleet item's place, by its family (the baseline table it came from), for
    /// `aterm machines here` and the agent.
    #[must_use]
    pub fn fleet(family: &str, remedy: Remedy) -> Self {
        let (layer, class) = match family {
            // `base.clt` is this family's item (design §4.2), installed as root.
            "developer" => (Layer::Base, Class::Elevate),
            // The app and each user's toolset, from the network.
            "aterm" => (Layer::Base, Class::Install),
            "app" | "homebrew" | "github" => (Layer::Workstation, Class::Install),
            "person" => (Layer::Workstation, Class::Person),
            "keyboard" | "consumer" | "setup" | "defaults" => {
                (Layer::Workstation, Class::Preference)
            }
            // names, sharing, firewall, time, power, updates: the Mac's own settings, as
            // root.
            _ => (Layer::Workstation, Class::Elevate),
        };
        Self {
            layer,
            class,
            requires: Requires::Account,
            remedy,
        }
    }
}

/// A machines [`Item`] with the `System` and context it runs against: an
/// [`aterm_converge::Item`]. It forwards the item's id, family, title, probe, apply,
/// refresh, activation, detail, revert, effect and stood-aside lines, and takes its layer,
/// class and precondition from its [`Place`].
pub struct Bound<'a> {
    item: &'a dyn Item,
    sys: &'a dyn System,
    ctx: &'a Ctx,
    place: Place,
    after: Vec<String>,
}

impl<'a> Bound<'a> {
    /// `item`, bound to `sys` and `ctx` at `place`, with its own `after` edges.
    #[must_use]
    pub fn new(item: &'a dyn Item, sys: &'a dyn System, ctx: &'a Ctx, place: Place) -> Self {
        Self {
            after: item.after(),
            item,
            sys,
            ctx,
            place,
        }
    }
}

/// Bind the items of one run. Each item's `after` is narrowed to the items the run holds:
/// a family names prerequisites from other baseline tables (`homebrew.pkg` after
/// `developer.command-line-tools`), and a baseline without that table, or an
/// `--only <table>` run (`baseline::keep_only`), leaves them out, which the machines
/// engine always read as no constraint. The converge engine refuses an edge that names no
/// item, so an edge is kept only when it names one here, by the engine's own rule
/// ([`aterm_converge::named_by`]).
#[must_use]
pub fn bind<'a>(
    items: impl IntoIterator<Item = (&'a dyn Item, Place)>,
    sys: &'a dyn System,
    ctx: &'a Ctx,
) -> Vec<Box<dyn aterm_converge::Item + 'a>> {
    let mut bound: Vec<Bound<'a>> = items
        .into_iter()
        .map(|(item, place)| Bound::new(item, sys, ctx, place))
        .collect();
    let ids: Vec<String> = bound.iter().map(|b| b.item.id()).collect();
    for (me, b) in bound.iter_mut().enumerate() {
        b.after
            .retain(|want| aterm_converge::named_by(want, me, &ids).next().is_some());
    }
    bound
        .into_iter()
        .map(|b| Box::new(b) as Box<dyn aterm_converge::Item + 'a>)
        .collect()
}

impl aterm_converge::Item for Bound<'_> {
    fn id(&self) -> String {
        self.item.id()
    }

    fn family(&self) -> &'static str {
        self.item.family()
    }

    fn layer(&self) -> Layer {
        self.place.layer
    }

    fn class(&self) -> Class {
        self.place.class
    }

    fn requires(&self) -> Requires {
        self.place.requires
    }

    fn title(&self) -> String {
        self.item.title()
    }

    fn probe(&self) -> Probe {
        self.item.probe(self.sys, self.ctx)
    }

    fn apply(&self) -> Result<(), String> {
        self.item.apply(self.sys, self.ctx)
    }

    fn refresh(&self) -> Option<Result<(), String>> {
        self.item.refresh(self.sys, self.ctx)
    }

    fn activation(&self) -> Option<Arc<dyn aterm_converge::Activation + '_>> {
        self.item.activation().map(|inner| {
            Arc::new(BoundActivation {
                inner,
                sys: self.sys,
                ctx: self.ctx,
            }) as Arc<dyn aterm_converge::Activation + '_>
        })
    }

    fn explain(&self, probe: &Probe) -> Explain {
        let (why, remedy) = match probe {
            Probe::Person { step, how } => (
                how.clone(),
                Remedy::Person {
                    pane: step.clone(),
                    how: how.clone(),
                },
            ),
            Probe::Ok { now } => (now.clone(), Remedy::None),
            // A missing precondition this item cannot lift; its line says which.
            Probe::NotApplicable { why } | Probe::Blocked { why } => (why.clone(), Remedy::None),
            Probe::Change { from, to } => {
                (format!("{from}; wanted: {to}"), self.place.remedy.clone())
            }
            // The surface's next run: after the prerequisite, when the Mac is idle, when
            // the read answers.
            Probe::Waits { why, .. } | Probe::Deferred { why } | Probe::Unusable { why } => {
                (why.clone(), self.place.remedy.clone())
            }
        };
        Explain { why, remedy }
    }

    fn after(&self) -> Vec<String> {
        self.after.clone()
    }

    fn detail(&self) -> Option<String> {
        self.item.detail(self.sys, self.ctx)
    }

    fn revert(&self) -> Option<String> {
        self.item.revert()
    }

    fn effect(&self) -> Effect {
        self.item.effect()
    }

    fn stood_aside(&self) -> Option<StoodAside> {
        self.item.stood_aside()
    }

    fn stands_aside(&self) -> Option<StoodAside> {
        self.item.stands_aside(self.sys, self.ctx)
    }
}

/// A machines [`Activation`] with the `System` and context it runs against.
struct BoundActivation<'a> {
    inner: Arc<dyn Activation>,
    sys: &'a dyn System,
    ctx: &'a Ctx,
}

impl aterm_converge::Activation for BoundActivation<'_> {
    fn id(&self) -> String {
        self.inner.id()
    }

    fn pending(&self) -> Result<Option<(String, String)>, String> {
        self.inner.pending(self.sys, self.ctx)
    }

    fn activate(&self) -> Result<Vec<(String, String)>, String> {
        self.inner.activate(self.sys, self.ctx)
    }
}

/// Run a fleet baseline's `items` (`aterm machines here`, the agent's run job), each placed
/// by its family with `remedy` as the surface's own command, handing each row to `on_row`
/// as soon as its item finishes: the agent's run job streams them to `progress.ndjson`
/// (design §4.4).
///
/// # Errors
/// The items cannot be ordered ([`aterm_converge::order`]); nothing was probed.
pub fn run_with(
    items: &[Box<dyn Item>],
    mode: Mode,
    sys: &dyn System,
    ctx: &Ctx,
    remedy: &Remedy,
    journal: &mut dyn Journal,
    on_row: &mut dyn FnMut(&ItemReport),
) -> Result<Vec<ItemReport>, Refusal> {
    let bound = bind(
        items
            .iter()
            .map(|item| (item.as_ref(), Place::fleet(item.family(), remedy.clone()))),
        sys,
        ctx,
    );
    aterm_converge::run_with(&bound, mode, journal, on_row)
}

/// [`run_with`] as the families' tests call it: placed as a fleet run, no stream, and a
/// composition that cannot be ordered fails the test.
#[cfg(test)]
pub fn run(
    items: &[Box<dyn Item>],
    mode: Mode,
    sys: &dyn System,
    ctx: &Ctx,
    journal: &mut dyn Journal,
) -> Vec<ItemReport> {
    run_with(items, mode, sys, ctx, &Remedy::None, journal, &mut |_| {})
        .unwrap_or_else(|refused| panic!("the test composed items no order runs: {refused}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HostFacts, Outcome};
    use crate::port::Fake;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

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

    /// A machines item that reads the `System` and context it is handed, so a test sees
    /// that the binding passes them through.
    struct Seen {
        id: &'static str,
        family: &'static str,
        probes: Mutex<Vec<Probe>>,
        after: Vec<String>,
        refreshes: Arc<AtomicU32>,
        refresh: Result<(), String>,
        aside: Option<StoodAside>,
        hosts: Arc<Mutex<Vec<String>>>,
    }

    impl Seen {
        fn new(id: &'static str, family: &'static str, probes: Vec<Probe>) -> Self {
            Self {
                id,
                family,
                probes: Mutex::new(probes),
                after: Vec::new(),
                refreshes: Arc::new(AtomicU32::new(0)),
                refresh: Ok(()),
                aside: None,
                hosts: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl Item for Seen {
        fn id(&self) -> String {
            self.id.into()
        }
        fn family(&self) -> &'static str {
            self.family
        }
        fn title(&self) -> String {
            self.id.into()
        }
        fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
            self.hosts
                .lock()
                .unwrap()
                .push(format!("{}@{}", ctx.host_name, sys.euid()));
            let mut p = self.probes.lock().unwrap();
            if p.len() > 1 {
                p.remove(0)
            } else {
                p[0].clone()
            }
        }
        fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
            Ok(())
        }
        fn refresh(&self, _: &dyn System, _: &Ctx) -> Option<Result<(), String>> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            Some(self.refresh.clone())
        }
        fn after(&self) -> Vec<String> {
            self.after.clone()
        }
        fn detail(&self, _: &dyn System, ctx: &Ctx) -> Option<String> {
            Some(format!("key for {}", ctx.host_name))
        }
        fn stood_aside(&self) -> Option<StoodAside> {
            self.aside.clone()
        }
    }

    fn ok() -> Probe {
        Probe::Ok { now: "b".into() }
    }

    fn change() -> Probe {
        Probe::Change {
            from: "a".into(),
            to: "b".into(),
        }
    }

    #[test]
    fn the_binding_forwards_family_refresh_detail_and_the_system_and_context() {
        let mut sys = Fake::root();
        sys.euid = 501;
        let item = Seen::new("keyboard.caps-lock@alice", "keyboard", vec![ok()]);
        let (refreshes, hosts) = (Arc::clone(&item.refreshes), Arc::clone(&item.hosts));
        let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
        let rows = run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Ok);
        // `--only <table>` merges a report by each row's family.
        assert_eq!(rows[0].family, "keyboard");
        assert_eq!(rows[0].detail.as_deref(), Some("key for h"));
        assert_eq!(rows[0].layer, Some(Layer::Workstation));
        assert_eq!(rows[0].class, Some(Class::Preference));
        // One refresh in an apply, and its read-back: both probes saw this sys and ctx.
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(*hosts.lock().unwrap(), ["h@501", "h@501"]);
    }

    #[test]
    fn a_stood_aside_toolset_apply_carries_its_kind_into_the_row() {
        let remedy = Remedy::Command("aterm machines apply h".into());
        for (aside, status, wanted) in [
            (
                StoodAside::Lock("another atpkg run held the store".into()),
                75,
                remedy.clone(),
            ),
            (
                StoodAside::Offline("the ALab index could not be reached".into()),
                69,
                Remedy::Network,
            ),
        ] {
            let mut item = Seen::new("aterm.toolset@alice", "aterm", vec![change(), ok()]);
            item.aside = Some(aside.clone());
            let hosts = Arc::clone(&item.hosts);
            let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
            let rows = run_with(
                &items,
                Mode::Apply,
                &Fake::root(),
                &ctx(),
                &remedy,
                &mut NoJournal,
                &mut |_| {},
            )
            .unwrap();
            assert_eq!(rows[0].outcome, Outcome::Blocked);
            assert_eq!(rows[0].why.as_deref(), Some(aside.why()));
            assert_eq!(
                rows[0].stood_aside.as_ref().map(StoodAside::exit_status),
                Some(status)
            );
            // Probed once: no `aterm pkg doctor` as the user after atpkg stood aside.
            assert_eq!(hosts.lock().unwrap().len(), 1, "{aside:?} was read back");
            // A lock is the surface's next run; offline is the network's.
            assert_eq!(
                rows[0].explain,
                Some(Explain {
                    why: aside.why().into(),
                    remedy: wanted,
                })
            );
        }
    }

    #[test]
    fn a_failed_refresh_is_explained_as_a_failure_the_next_run_retries() {
        // init's trackpad: the saved preference reads as wanted and the native refresh
        // refuses. The row fails, and `aterm init` again is what retries it; "nothing
        // aterm can do" would be the Ok read's explanation.
        let mut item = Seen::new(
            "init.trackpad.scroll-direction@alice",
            "trackpad",
            vec![ok()],
        );
        item.refresh = Err("the native setter refused".into());
        let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
        let remedy = Remedy::Command("aterm init".into());
        let rows = run_with(
            &items,
            Mode::Apply,
            &Fake::root(),
            &ctx(),
            &remedy,
            &mut NoJournal,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(rows[0].outcome, Outcome::Failed);
        assert_eq!(
            rows[0].explain,
            Some(Explain {
                why: "the native setter refused".into(),
                remedy,
            })
        );
    }

    #[test]
    fn an_edge_to_a_table_the_run_does_not_hold_is_no_constraint_and_one_it_holds_orders() {
        // `apply --only homebrew`: no developer table, so no Command Line Tools item.
        let mut brew = Seen::new("homebrew.pkg", "homebrew", vec![ok()]);
        brew.after = vec!["developer.command-line-tools".into()];
        let items: Vec<Box<dyn Item>> = vec![Box::new(brew)];
        let rows = run(&items, Mode::Apply, &Fake::root(), &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Ok);
        // With the developer table, composed after it, the edge orders the run and blocks.
        let mut brew = Seen::new("homebrew.pkg", "homebrew", vec![ok()]);
        brew.after = vec!["developer.*".into()];
        let clt = Seen::new(
            "developer.command-line-tools",
            "developer",
            vec![Probe::Unusable { why: "x".into() }],
        );
        let items: Vec<Box<dyn Item>> = vec![Box::new(brew), Box::new(clt)];
        let rows = run(&items, Mode::Apply, &Fake::root(), &ctx(), &mut NoJournal);
        assert_eq!(rows[0].id, "developer.command-line-tools");
        assert_eq!(rows[0].layer, Some(Layer::Base));
        assert_eq!(rows[0].class, Some(Class::Elevate));
        assert_eq!(rows[1].outcome, Outcome::Blocked);
        assert_eq!(
            rows[1].why.as_deref(),
            Some("waits for developer.command-line-tools")
        );
        // Two items that wait on each other are still refused.
        let mut a = Seen::new("names.a", "names", vec![ok()]);
        a.after = vec!["names.b".into()];
        let mut b = Seen::new("names.b", "names", vec![ok()]);
        b.after = vec!["names.a".into()];
        let items: Vec<Box<dyn Item>> = vec![Box::new(a), Box::new(b)];
        let refused = run_with(
            &items,
            Mode::Plan,
            &Fake::root(),
            &ctx(),
            &Remedy::None,
            &mut NoJournal,
            &mut |_| {},
        );
        assert!(matches!(refused, Err(Refusal::Cycle { .. })));
    }

    #[test]
    fn a_change_and_a_person_step_are_explained_with_the_surfaces_remedy_and_the_pane() {
        let items: Vec<Box<dyn Item>> = vec![
            Box::new(Seen::new("power.sleep", "power", vec![change()])),
            Box::new(Seen::new(
                "person.fda",
                "person",
                vec![Probe::Person {
                    step: "fda".into(),
                    how: "Privacy & Security ▸ Full Disk Access".into(),
                }],
            )),
        ];
        let remedy = Remedy::Command("aterm machines apply h".into());
        let rows = run_with(
            &items,
            Mode::Plan,
            &Fake::root(),
            &ctx(),
            &remedy,
            &mut NoJournal,
            &mut |_| {},
        )
        .unwrap();
        let explain = rows[0].explain.as_ref().unwrap();
        assert_eq!(explain.remedy, remedy);
        assert_eq!(explain.why, "a; wanted: b");
        assert_eq!(rows[1].class, Some(Class::Person));
        assert_eq!(
            rows[1].explain.as_ref().unwrap().remedy,
            Remedy::Person {
                pane: "fda".into(),
                how: "Privacy & Security ▸ Full Disk Access".into()
            }
        );
    }
}

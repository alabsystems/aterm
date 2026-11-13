// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Convergence engine binding for machines items ([`Bound`]).

use crate::items::{Activation, Ctx, Item};
use crate::model::{ItemReport, Mode, Probe};
use crate::port::System;
use aterm_converge::{Class, Effect, Explain, Layer, Remedy, Requires, StoodAside};
use std::sync::Arc;

#[cfg(any(test, debug_assertions))]
pub use aterm_converge::NoJournal;
pub use aterm_converge::{Journal, Refusal};

/// Where a machines item sits in the desired state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// The layer.
    pub layer: Layer,
    /// What its apply does.
    pub class: Class,
    /// Where it may run.
    pub requires: Requires,
    /// The act that makes a change of this run's.
    pub remedy: Remedy,
}

impl Place {
    /// A fleet item's place, by its family.
    #[must_use]
    pub fn fleet(family: &str, remedy: Remedy) -> Self {
        let (layer, class) = match family {
            "developer" => (Layer::Base, Class::Elevate),
            "aterm" => (Layer::Base, Class::Install),
            "app" | "homebrew" | "github" => (Layer::Workstation, Class::Install),
            "person" => (Layer::Workstation, Class::Person),
            "keyboard" | "consumer" | "setup" | "defaults" => {
                (Layer::Workstation, Class::Preference)
            }
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

/// A machines [`Item`] bound to its `System` and context ([`aterm_converge::Item`], [`Place`]).
pub struct Bound<'a> {
    item: &'a dyn Item,
    sys: &'a dyn System,
    ctx: &'a Ctx,
    place: Place,
    after: Vec<String>,
}

fn continuing(sys: &dyn System) -> Result<(), String> {
    match sys.stopped() {
        Some(signal) => Err(format!(
            "setup was stopped by {}",
            crate::port::stop_name(signal)
        )),
        None => Ok(()),
    }
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

/// Bind the items of one run ([`aterm_converge::named_by`]).
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
        continuing(self.sys)?;
        self.item
            .apply_guarded(self.sys, self.ctx, &|| continuing(self.sys))
    }

    fn refresh(&self) -> Option<Result<(), String>> {
        self.item
            .refresh_guarded(self.sys, self.ctx, &|| continuing(self.sys))
    }

    fn refresh_changes(&self) -> bool {
        self.item.refresh_changes(self.sys, self.ctx)
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
            Probe::NotApplicable { why } | Probe::Blocked { why } => (why.clone(), Remedy::None),
            Probe::Change { from, to } => {
                (format!("{from}; wanted: {to}"), self.place.remedy.clone())
            }
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

    fn read_aside(&self) -> Option<StoodAside> {
        self.item.read_aside()
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
        continuing(self.sys)?;
        self.inner.activate(self.sys, self.ctx)
    }
}

/// Run a fleet baseline's `items`.
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

/// [`run_with`] as the families' tests call it.
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
#[path = "engine_tests.rs"]
mod tests;

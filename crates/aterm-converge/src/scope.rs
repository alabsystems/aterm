// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Execution scope and `--only` item selection for convergence runs.

use crate::Item;
use crate::model::{Class, Effect, Explain, Layer, Probe, Requires, StoodAside};

/// Execution scope for a convergence run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scope {
    /// The classes an apply run applies.
    pub applies: &'static [Class],
    /// Whether the run's user owns `/dev/console`.
    pub at_console: bool,
    /// Why the run's process is not a shell of its user's own account, or `None` in one.
    pub not_own_account: Option<&'static str>,
}

impl Scope {
    /// Every class.
    pub const EVERY_CLASS: &'static [Class] = &[
        Class::Relay,
        Class::Asked,
        Class::Install,
        Class::Elevate,
        Class::Person,
        Class::Preference,
    ];

    /// The two repair classes: what bare `aterm doctor` and `aterm pkg repair` apply.
    pub const REPAIR_CLASSES: &'static [Class] = &[Class::Relay, Class::Asked];

    /// Bare `aterm doctor`'s and `aterm pkg repair`'s scope in the account's own shell.
    pub const REPAIR: Self = Self {
        applies: Self::REPAIR_CLASSES,
        at_console: true,
        not_own_account: None,
    };

    /// Every class, at the console, in the account's own shell.
    pub const EVERY: Self = Self {
        applies: Self::EVERY_CLASS,
        at_console: true,
        not_own_account: None,
    };

    /// Whether an apply run applies an item of `class`.
    #[inline]
    #[must_use]
    pub fn applies(self, class: Class) -> bool {
        self.applies.contains(&class)
    }

    /// Whether an item that `requires` this may change anything here.
    #[inline]
    #[must_use]
    pub const fn admits(self, requires: Requires) -> bool {
        match requires {
            Requires::Account => self.not_own_account.is_none(),
            Requires::Console => self.not_own_account.is_none() && self.at_console,
        }
    }
}

/// The line a [`Requires::Console`] item's row carries where the run's user is not at this Mac's
/// console (design §3.2: base and develop items run over SSH; workstation items do not).
pub const NOT_AT_CONSOLE: &str = "run at this Mac's console: this login does not own it \
                                  (another user is logged in there, or this is a remote \
                                  shell)";

/// Format the blocked reason when not running in the user's own account shell.
#[must_use]
pub fn not_own_account(why: &str) -> String {
    format!(
        "not changed from this shell ({why}): run it as yourself, in your own login and home, \
         without sudo"
    )
}

impl Layer {
    /// The layer's name, as a report and `--only` spell it.
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Workstation => "workstation",
            Self::Develop => "develop",
            Self::Release => "release",
        }
    }
}

/// Whether `--only` token `token` matches an item by family, layer, id, or prefix.
#[must_use]
pub fn selects(token: &str, id: &str, family: &str, layer: Option<Layer>) -> bool {
    if token.is_empty() {
        return false;
    }
    token == family
        || layer.is_some_and(|layer| layer.name() == token)
        || id.strip_prefix(token).is_some_and(|rest| {
            rest.is_empty() || (!token.contains('@') && rest.starts_with(['.', '@']))
        })
}

/// Filter `items` by `tokens` in composition order and return unmatched tokens.
#[must_use]
pub fn select<'a>(
    items: Vec<Box<dyn Item + 'a>>,
    tokens: &[String],
) -> (Vec<Box<dyn Item + 'a>>, Vec<String>) {
    if tokens.is_empty() {
        return (items, Vec::new());
    }
    let picks = |item: &dyn Item, token: &str| {
        selects(token, &item.id(), item.family(), Some(item.layer()))
    };
    let unmatched: Vec<String> = tokens
        .iter()
        .filter(|token| !items.iter().any(|item| picks(item.as_ref(), token)))
        .cloned()
        .collect();
    let kept: Vec<Box<dyn Item + 'a>> = items
        .into_iter()
        .filter(|item| tokens.iter().any(|token| picks(item.as_ref(), token)))
        .collect();
    let ids: Vec<String> = kept.iter().map(|item| item.id()).collect();
    let narrowed = kept
        .into_iter()
        .enumerate()
        .map(|(me, inner)| {
            let after = inner
                .after()
                .into_iter()
                .filter(|want| crate::named_by(want, me, &ids).next().is_some())
                .collect();
            Box::new(Narrowed { inner, after }) as Box<dyn Item + 'a>
        })
        .collect();
    (narrowed, unmatched)
}

/// `items` whose apply stands aside with `aside` instead of acting.
#[must_use]
pub fn standing_aside<'a>(
    items: Vec<Box<dyn Item + 'a>>,
    aside: &StoodAside,
) -> Vec<Box<dyn Item + 'a>> {
    items
        .into_iter()
        .map(|inner| {
            Box::new(Aside {
                inner,
                aside: aside.clone(),
            }) as Box<dyn Item + 'a>
        })
        .collect()
}

struct Aside<'a> {
    inner: Box<dyn Item + 'a>,
    aside: StoodAside,
}

impl Item for Aside<'_> {
    fn id(&self) -> String {
        self.inner.id()
    }
    fn family(&self) -> &'static str {
        self.inner.family()
    }
    fn layer(&self) -> Layer {
        self.inner.layer()
    }
    fn class(&self) -> Class {
        self.inner.class()
    }
    fn requires(&self) -> Requires {
        self.inner.requires()
    }
    fn title(&self) -> String {
        self.inner.title()
    }
    fn probe(&self) -> Probe {
        self.inner.probe()
    }
    fn apply(&self) -> Result<(), String> {
        Ok(())
    }
    fn activation(&self) -> Option<std::sync::Arc<dyn crate::Activation + '_>> {
        self.inner.activation()
    }
    fn explain(&self, probe: &Probe) -> Explain {
        self.inner.explain(probe)
    }
    fn essential(&self) -> bool {
        self.inner.essential()
    }
    fn after(&self) -> Vec<String> {
        self.inner.after()
    }
    fn detail(&self) -> Option<String> {
        self.inner.detail()
    }
    fn revert(&self) -> Option<String> {
        self.inner.revert()
    }
    fn effect(&self) -> Effect {
        self.inner.effect()
    }
    fn stands_aside(&self) -> Option<StoodAside> {
        Some(self.aside.clone())
    }
    fn read_aside(&self) -> Option<StoodAside> {
        self.inner.read_aside()
    }
}

/// Wrap `items` so pending changes report `Blocked` with `why` instead of applying.
#[must_use]
pub fn held_back<'a>(items: Vec<Box<dyn Item + 'a>>, why: &str) -> Vec<Box<dyn Item + 'a>> {
    hold(items, why, true, false)
}

/// `items` a run leaves as they are now.
#[must_use]
pub fn left_as_is<'a>(items: Vec<Box<dyn Item + 'a>>, why: &str) -> Vec<Box<dyn Item + 'a>> {
    hold(items, why, true, true)
}

/// `items` another run is laying now (an install pass holding the store): each is left as it is,
/// as [`left_as_is`] leaves it, and waits on no other item, since it changes nothing; one whose
/// probe waits on another item (that run lays both) reads as left too.
#[must_use]
pub fn left_to_another<'a>(items: Vec<Box<dyn Item + 'a>>, why: &str) -> Vec<Box<dyn Item + 'a>> {
    hold(items, why, false, true)
}

fn hold<'a>(
    items: Vec<Box<dyn Item + 'a>>,
    why: &str,
    waits: bool,
    as_is: bool,
) -> Vec<Box<dyn Item + 'a>> {
    items
        .into_iter()
        .map(|inner| {
            Box::new(HeldBack {
                inner,
                why: why.to_owned(),
                waits,
                as_is,
            }) as Box<dyn Item + 'a>
        })
        .collect()
}

struct HeldBack<'a> {
    inner: Box<dyn Item + 'a>,
    why: String,
    /// The item's own `after`: false for [`left_to_another`], which waits on nothing.
    waits: bool,
    /// [`left_as_is`]: a change reads `NotApplicable`, not `Blocked`.
    as_is: bool,
}

impl Item for HeldBack<'_> {
    fn id(&self) -> String {
        self.inner.id()
    }
    fn family(&self) -> &'static str {
        self.inner.family()
    }
    fn layer(&self) -> Layer {
        self.inner.layer()
    }
    fn class(&self) -> Class {
        self.inner.class()
    }
    fn requires(&self) -> Requires {
        self.inner.requires()
    }
    fn title(&self) -> String {
        self.inner.title()
    }
    fn probe(&self) -> Probe {
        match self.inner.probe() {
            Probe::Change { from, .. } if self.as_is => Probe::NotApplicable {
                why: format!("{from}; left as it is: {}", self.why),
            },
            Probe::Change { from, .. } => Probe::Blocked {
                why: format!("{from}; not changed: {}", self.why),
            },
            Probe::Waits { on, .. } if !self.waits => Probe::NotApplicable {
                why: format!("waits for {on}; left as it is: {}", self.why),
            },
            probe => probe,
        }
    }
    fn apply(&self) -> Result<(), String> {
        Err(self.why.clone())
    }
    fn activation(&self) -> Option<std::sync::Arc<dyn crate::Activation + '_>> {
        if self.waits {
            self.inner.activation()
        } else {
            None
        }
    }
    fn explain(&self, probe: &Probe) -> Explain {
        self.inner.explain(probe)
    }
    fn essential(&self) -> bool {
        self.inner.essential()
    }
    fn after(&self) -> Vec<String> {
        if self.waits {
            self.inner.after()
        } else {
            Vec::new()
        }
    }
    fn detail(&self) -> Option<String> {
        self.inner.detail()
    }
    fn effect(&self) -> Effect {
        self.inner.effect()
    }
    fn read_aside(&self) -> Option<StoodAside> {
        self.inner.read_aside()
    }
}

struct Narrowed<'a> {
    inner: Box<dyn Item + 'a>,
    after: Vec<String>,
}

impl Item for Narrowed<'_> {
    fn id(&self) -> String {
        self.inner.id()
    }
    fn family(&self) -> &'static str {
        self.inner.family()
    }
    fn layer(&self) -> Layer {
        self.inner.layer()
    }
    fn class(&self) -> Class {
        self.inner.class()
    }
    fn requires(&self) -> Requires {
        self.inner.requires()
    }
    fn title(&self) -> String {
        self.inner.title()
    }
    fn probe(&self) -> Probe {
        self.inner.probe()
    }
    fn apply(&self) -> Result<(), String> {
        self.inner.apply()
    }
    fn refresh(&self) -> Option<Result<(), String>> {
        self.inner.refresh()
    }
    fn refresh_changes(&self) -> bool {
        self.inner.refresh_changes()
    }
    fn activation(&self) -> Option<std::sync::Arc<dyn crate::Activation + '_>> {
        self.inner.activation()
    }
    fn explain(&self, probe: &Probe) -> Explain {
        self.inner.explain(probe)
    }
    fn essential(&self) -> bool {
        self.inner.essential()
    }
    fn after(&self) -> Vec<String> {
        self.after.clone()
    }
    fn detail(&self) -> Option<String> {
        self.inner.detail()
    }
    fn revert(&self) -> Option<String> {
        self.inner.revert()
    }
    fn effect(&self) -> Effect {
        self.inner.effect()
    }
    fn stood_aside(&self) -> Option<StoodAside> {
        self.inner.stood_aside()
    }
    fn stands_aside(&self) -> Option<StoodAside> {
        self.inner.stands_aside()
    }
    fn read_aside(&self) -> Option<StoodAside> {
        self.inner.read_aside()
    }
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;

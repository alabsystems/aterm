// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Item families and [`expand`].

use crate::baseline::Baseline;
use crate::model::{Effect, HostFacts, PValue, Probe, Read, User};
use crate::port::System;
use std::path::PathBuf;
use std::sync::Arc;

pub mod apps;
pub mod aterm;
pub mod catalog;
pub mod defaults;
pub mod developer;
pub mod firewall;
pub mod github;
pub mod homebrew;
pub mod keyboard;
pub mod lan;
pub mod names;
pub mod person;
pub mod power;
pub mod setup;
pub mod sharing;
pub mod time;
pub mod updates;
pub mod widgets;

/// Host context shared across items.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// The host.
    pub facts: HostFacts,
    /// Target user accounts.
    pub users: Vec<User>,
    /// Console user account, if any.
    pub console: Option<User>,
    /// Inventory name of this host.
    pub host_name: String,
    /// Root-owned verified artifact cache directory.
    pub cache_dir: PathBuf,
    /// Whether disruptive changes are allowed.
    pub disrupt: bool,
}

impl Ctx {
    /// Returns the cached artifact path for `sha256`, if present.
    #[must_use]
    pub fn cached(&self, sha256: &str) -> Option<PathBuf> {
        self.facts
            .cached
            .iter()
            .any(|c| c.eq_ignore_ascii_case(sha256))
            .then(|| self.cache_dir.join(sha256.to_ascii_lowercase()))
    }
}

/// Shared activation step across items.
pub trait Activation: Send + Sync {
    /// Stable id within a run.
    fn id(&self) -> String;

    /// Pending journal `(from, to)`, or `None` when nothing awaits activation.
    ///
    /// # Errors
    /// One line when what awaits it cannot be read.
    fn pending(&self, sys: &dyn System, ctx: &Ctx) -> Result<Option<(String, String)>, String>;

    /// Applies pending changes and returns failed `(id, reason)` pairs.
    ///
    /// # Errors
    /// One line when none of it took effect; what awaits it stays for the next run.
    fn activate(&self, sys: &dyn System, ctx: &Ctx) -> Result<Vec<(String, String)>, String>;
}

/// One setting the engine probes, plans, applies, and verifies.
pub trait Item: Send + Sync {
    /// Stable id within a run.
    fn id(&self) -> String;

    /// Baseline table family name.
    fn family(&self) -> &'static str;

    /// One human line for the report.
    fn title(&self) -> String;

    /// Reads the host and decides without mutating state.
    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe;

    /// Apply the change planned by [`Item::probe`].
    ///
    /// # Errors
    /// One line saying what did not work.
    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String>;

    /// Apply while a caller's current authorization still holds. Items that perform a batch
    /// of effects check again at each boundary rather than carrying one admission through it.
    ///
    /// # Errors
    /// The caller withdraws authorization, or the item's apply cannot finish.
    fn apply_guarded(
        &self,
        sys: &dyn System,
        ctx: &Ctx,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        current()?;
        self.apply(sys, ctx)
    }

    /// Typed before-images of preference keys this item currently plans to change.
    ///
    /// # Errors
    /// A preference that would be overwritten cannot be read safely.
    fn preference_changes(
        &self,
        _sys: &dyn System,
        _ctx: &Ctx,
    ) -> Result<Vec<(defaults::Target, Read<PValue>)>, String> {
        Ok(Vec::new())
    }

    /// Reapply an idempotent runtime setting after an `Ok` probe in apply mode, while the
    /// caller's live authorization holds. A runtime refresh checks `current` immediately
    /// before its effect, and a batch checks again at each boundary. An item without a
    /// refresh returns `None` without consuming or changing that authorization.
    fn refresh_guarded(
        &self,
        _sys: &dyn System,
        _ctx: &Ctx,
        _current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Option<Result<(), String>> {
        None
    }

    /// Whether an apply-mode refresh must be journaled before it runs.
    fn refresh_changes(&self, _sys: &dyn System, _ctx: &Ctx) -> bool {
        false
    }

    /// Command that undoes `apply`, if any.
    fn revert(&self) -> Option<String> {
        None
    }

    /// When the change takes effect.
    fn effect(&self) -> Effect {
        Effect::Now
    }

    /// Shared [`Activation`] run before read-back.
    fn activation(&self) -> Option<Arc<dyn Activation>> {
        None
    }

    /// Prerequisite item ids or `<prefix>*` patterns.
    fn after(&self) -> Vec<String> {
        Vec::new()
    }

    /// Family-specific post-run controller output.
    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        None
    }

    /// Reason the last [`Item::apply`] stood aside.
    fn stood_aside(&self) -> Option<aterm_converge::StoodAside> {
        None
    }

    /// Why the last probe or apply left the Command Line Tools uninstalled, in the class
    /// `aterm machines here apply --plain` exits with (`developer.command-line-tools` alone).
    fn unfinished(&self) -> Option<crate::init_clt::Unfinished> {
        None
    }

    /// Reason an `Unusable` probe was blocked by a missing prerequisite.
    fn read_aside(&self) -> Option<aterm_converge::StoodAside> {
        None
    }

    /// Reason this item skipped sandboxed keys without Full Disk Access.
    fn withheld(&self) -> Option<String> {
        None
    }

    /// Reason this item stands aside before applying.
    fn stands_aside(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<aterm_converge::StoodAside> {
        None
    }

    /// Whether `from`, what this item read as it found a change ([`Probe::Change`]), changes
    /// only targets that `held`, what it read back the last time it read as wanted, does not
    /// name: a key macOS added since (a keyboard model it lists for the first time), not a
    /// change of anything aterm set. An item whose targets never grow says no.
    fn new_targets(&self, _held: &str, _from: &str) -> bool {
        false
    }
}

/// Expands every item requested by `baseline` in execution order.
///
/// # Errors
/// One line naming the table that cannot be expanded.
pub fn expand(baseline: &Baseline, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    // A process that reads several rows' keys is relaunched once, after the last of them.
    let relaunches = defaults::Relaunches::default();
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    if let Some(t) = &baseline.names {
        items.extend(names::expand(t, ctx)?);
    }
    if let Some(t) = &baseline.sharing {
        items.extend(sharing::expand(t, ctx)?);
    }
    if let Some(t) = &baseline.firewall {
        items.extend(firewall::expand(t, ctx)?);
    }
    if let Some(t) = &baseline.time {
        items.extend(time::expand(t, ctx)?);
    }
    if let Some(t) = &baseline.power {
        items.extend(power::expand(t, ctx)?);
    }
    if let Some(t) = &baseline.updates {
        items.extend(updates::expand(t, ctx)?);
    }
    if let Some(t) = &baseline.keyboard {
        items.extend(keyboard::expand(t, ctx)?);
    }
    if let Some(t) = &baseline.consumer {
        items.extend(catalog::expand_with(t, ctx, &relaunches)?);
    }
    if let Some(t) = &baseline.setup {
        items.extend(setup::expand(t, ctx)?);
    }
    items.extend(defaults::expand_rows_with(
        &baseline.defaults,
        ctx,
        &relaunches,
    )?);
    // The broker before the developer table's Command Line Tools: their download can take
    // minutes, and the vault look waits on the broker.
    if let Some(t) = &baseline.lan {
        items.extend(lan::expand(t, ctx)?);
    }
    if let Some(t) = &baseline.developer {
        items.extend(developer::expand(t, ctx)?);
    }
    if let Some(t) = &baseline.homebrew {
        items.extend(homebrew::expand(t, ctx)?);
    }
    items.extend(apps::expand(&baseline.apps, ctx)?);
    if let Some(t) = &baseline.aterm {
        items.extend(aterm::expand(t, ctx)?);
    }
    if let Some(t) = &baseline.github {
        items.extend(github::expand(t, ctx)?);
    }
    if let Some(t) = &baseline.person {
        items.extend(person::expand(t, ctx)?);
    }
    let mut seen = std::collections::BTreeSet::new();
    for item in &items {
        if !seen.insert(item.id()) {
            return Err(format!("two items share the id {}", item.id()));
        }
    }
    Ok(items)
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

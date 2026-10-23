// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Item families: each turns its baseline table into [`Item`]s, and [`expand`] collects them in run order.

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
pub mod names;
pub mod person;
pub mod power;
pub mod setup;
pub mod sharing;
pub mod time;
pub mod updates;

/// What every item may consult.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// The host.
    pub facts: HostFacts,
    /// The accounts user-scope items apply to, resolved (the baseline's `users`).
    pub users: Vec<User>,
    /// The account at the console, if any.
    pub console: Option<User>,
    /// The inventory name of this host (what `{name}` filled).
    pub host_name: String,
    /// The agent's root-owned artifact cache: `<sha256 hex>` files, each verified.
    pub cache_dir: PathBuf,
    /// Whether a change may disrupt someone using the Mac (`false` defers disruptive items with [`Probe::Deferred`]).
    pub disrupt: bool,
}

impl Ctx {
    /// The cached artifact with `sha256`, if the cache holds it.
    #[must_use]
    pub fn cached(&self, sha256: &str) -> Option<PathBuf> {
        self.facts
            .cached
            .iter()
            .any(|c| c.eq_ignore_ascii_case(sha256))
            .then(|| self.cache_dir.join(sha256.to_ascii_lowercase()))
    }
}

/// Shared activation step across items, adapted by [`crate::engine::Bound`] to [`aterm_converge::Activation`].
pub trait Activation: Send + Sync {
    /// Stable id, unique within a run: its journal line's.
    fn id(&self) -> String;

    /// Pending journal `from` and `to`, or `None` when nothing awaits activation.
    ///
    /// # Errors
    /// One line when what awaits it cannot be read.
    fn pending(&self, sys: &dyn System, ctx: &Ctx) -> Result<Option<(String, String)>, String>;

    /// Make applied changes take effect and return any failed `(id, reason)` pairs ([`aterm_converge::Activation::activate`]).
    ///
    /// # Errors
    /// One line when none of it took effect; what awaits it stays for the next run.
    fn activate(&self, sys: &dyn System, ctx: &Ctx) -> Result<Vec<(String, String)>, String>;
}

/// One setting the engine probes, plans, applies, and verifies, adapted by [`crate::engine::Bound`] to [`aterm_converge::Item`].
pub trait Item: Send + Sync {
    /// Stable id, unique within a run (e.g. `power.sleep`, `defaults.com.apple.dock.autohide@alice`).
    fn id(&self) -> String;

    /// The family, as the baseline table names it.
    fn family(&self) -> &'static str;

    /// One human line for the report.
    fn title(&self) -> String;

    /// Read the host and decide. Must change nothing.
    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe;

    /// Apply the change planned by [`Item::probe`].
    ///
    /// # Errors
    /// One line saying what did not work.
    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String>;

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

    /// Reapply an idempotent runtime setting after an `Ok` probe in apply mode.
    fn refresh(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<Result<(), String>> {
        None
    }

    /// The command that undoes `apply`, if one exists.
    fn revert(&self) -> Option<String> {
        None
    }

    /// When the change is felt.
    fn effect(&self) -> Effect {
        Effect::Now
    }

    /// Shared [`Activation`] run before read-back ([`aterm_converge::Item::activation`]).
    fn activation(&self) -> Option<Arc<dyn Activation>> {
        None
    }

    /// Item ids (or `<prefix>*` patterns) that must finish `Ok` or `Changed` before this item applies.
    fn after(&self) -> Vec<String> {
        Vec::new()
    }

    /// Family-specific output for the controller, read after the run (a public key).
    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        None
    }

    /// Reason the last [`Item::apply`] stood aside without changing anything or failing (`Blocked`).
    fn stood_aside(&self) -> Option<aterm_converge::StoodAside> {
        None
    }

    /// Reason an `Unusable` probe was blocked by a missing prerequisite ([`aterm_converge::Item::read_aside`]).
    fn read_aside(&self) -> Option<aterm_converge::StoodAside> {
        None
    }

    /// Reason this item skipped keys in another app's sandbox container without Full Disk Access ([`defaults::withheld`]).
    fn withheld(&self) -> Option<String> {
        None
    }

    /// Reason this item stands aside before applying ([`aterm_converge::Item::stands_aside`]).
    fn stands_aside(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<aterm_converge::StoodAside> {
        None
    }
}

/// Expand every item requested by `baseline` in dependency and execution order.
///
/// # Errors
/// One line naming the table that cannot be expanded.
pub fn expand(baseline: &Baseline, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
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
        items.extend(catalog::expand(t, ctx)?);
    }
    if let Some(t) = &baseline.setup {
        items.extend(setup::expand(t, ctx)?);
    }
    items.extend(defaults::expand_rows(&baseline.defaults, ctx)?);
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
mod tests {
    use super::*;
    use crate::items::defaults::tests::{ctx, user};

    #[test]
    fn every_item_is_of_the_family_its_baseline_table_names() {
        // `plan|apply --only <table>` merges a report into the saved one by each row's family,
        // so an item's family is the name of the table it came from.
        let text = crate::baseline::fill(
            crate::controller::templates::DEFAULT_BASELINE,
            &[
                ("name", "lab-07"),
                ("tag", "lab-07"),
                ("serial", "ZNYVNTRJN5"),
                ("uuid", "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43"),
            ],
        )
        .unwrap();
        let baseline = crate::baseline::parse(&text).unwrap();
        let items = expand(&baseline, &ctx(&[user("admin", 501)])).unwrap();
        let tables = crate::baseline::tables();
        let mut seen: Vec<&str> = Vec::new();
        for item in &items {
            assert!(
                tables.contains(&item.family()),
                "{} is of family {}, which no table is named",
                item.id(),
                item.family()
            );
            if !seen.contains(&item.family()) {
                seen.push(item.family());
            }
        }
        assert!(seen.len() >= 10, "the starter baseline's tables: {seen:?}");
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The item families (design §5.1). A family turns its baseline table into [`Item`]s;
//! [`expand`] collects them all, in the order they must run.

use crate::baseline::Baseline;
use crate::model::{Effect, HostFacts, PValue, Probe, Read, User};
use crate::port::System;
use std::path::PathBuf;

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
    /// May a change disrupt someone using the Mac (relaunch their Dock, swap an app they
    /// have open)? True when nobody is at the console, the console has been idle past
    /// the envelope's threshold, or the envelope says `now`. Disruptive items probe
    /// [`Probe::Deferred`] otherwise.
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

/// One setting the engine probes, plans, applies and verifies, handed the `System` and
/// context it runs against on each call; [`crate::engine::Bound`] makes it an
/// [`aterm_converge::Item`].
pub trait Item: Send + Sync {
    /// Stable id, unique within a run (e.g. `power.sleep`, `defaults.com.apple.dock.autohide@alice`).
    fn id(&self) -> String;

    /// The family, as the baseline table names it.
    fn family(&self) -> &'static str;

    /// One human line for the report.
    fn title(&self) -> String;

    /// Read the host and decide. Must change nothing.
    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe;

    /// Make the change the last [`Item::probe`] planned. The engine probes again afterwards,
    /// and only a matching read-back counts.
    ///
    /// # Errors
    /// One line saying what did not work.
    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String>;

    /// Typed before-images of the preference keys this item currently plans to
    /// change. A caller can persist these before running a workstation repair;
    /// account credentials and runtime-only input mappings are never included.
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

    /// Reapply an idempotent runtime setting whose saved preferences already match.
    /// Called only in apply mode after an `Ok` probe. `Some` requires another matching
    /// probe; `None` preserves the ordinary already-correct path without another read.
    /// This must not change the user's saved choice or install background work.
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

    /// Ids of items that must end `Ok` or `Changed` before this one may apply (Command
    /// Line Tools before Homebrew). An id ending in `*` names every item with that prefix.
    fn after(&self) -> Vec<String> {
        Vec::new()
    }

    /// Family-specific output for the controller, read after the run (a public key).
    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        None
    }

    /// Why the last [`Item::apply`] stood aside without changing anything and without
    /// failing: a lock another run holds, a network the change needs that is down
    /// (atpkg's exits 75 and 69). The engine then reports the item `Blocked` with this
    /// line rather than probing it again. `None`, the default, when the apply did its
    /// work or returned an error.
    fn stood_aside(&self) -> Option<aterm_converge::StoodAside> {
        None
    }
}

/// Every item `baseline` asks for, in run order: tools before what needs them (Command
/// Line Tools before Homebrew, Homebrew before its formulae, aterm before its person
/// steps), and settings before the person steps that close a run.
///
/// # Errors
/// One line naming the table that cannot be expanded (for example a user it names that
/// does not exist on this host).
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

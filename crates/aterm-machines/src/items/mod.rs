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

    /// Reapplies an idempotent runtime setting after an `Ok` probe in apply mode.
    fn refresh(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<Result<(), String>> {
        None
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
    use crate::items::defaults::tests::{Session, ctx, session_of, user};
    use crate::model::{Mode, Outcome};

    /// A `[consumer]` row and a `[[defaults]]` row that name one restart share it across
    /// their tables: `expand` hands both one registry, so the process is relaunched once,
    /// after the later write, under one journal line that names both rows. Negative
    /// control: the `[[defaults]]` rows given a registry of their own make a second
    /// relaunch of the same id, which the engine runs in place of the first, so its line
    /// names the `[[defaults]]` row alone and the catalog row's write is never relaunched
    /// for.
    #[test]
    fn a_catalog_row_and_a_defaults_row_of_one_reader_relaunch_it_once() {
        let admin = user("admin", 501);
        let text = format!(
            "schema = {}\n\n[consumer]\nairdrop = false\n\n[[defaults]]\n\
             domain = \"com.apple.sharingd\"\nkey = \"SuppressAirDropNotifications\"\n\
             value = true\nscope = \"user\"\nrestart = \"sharingd\"\n",
            crate::SCHEMA
        );
        let c = ctx(std::slice::from_ref(&admin));
        let items = expand(&crate::baseline::parse(&text).unwrap(), &c).unwrap();
        assert_eq!(
            items.len(),
            2,
            "{:?}",
            items.iter().map(|i| i.id()).collect::<Vec<_>>()
        );
        let (sys, session) = session_of(Session::running(&["sharingd"]));
        let mut journal = Recorded::default();
        let rows = crate::engine::run(&items, Mode::Apply, &sys, &c, &mut journal);
        assert!(
            rows.iter().all(|row| row.outcome == Outcome::Changed),
            "{rows:?}"
        );
        let session = session.lock().unwrap();
        assert_eq!(session.kills("sharingd"), 1, "{:?}", session.events);
        assert_eq!(
            session.events.last().map(String::as_str),
            Some("kill sharingd"),
            "{:?}",
            session.events
        );
        let relaunches: Vec<&(String, String)> = journal
            .0
            .iter()
            .filter(|(id, _)| id == "relaunch.sharingd@admin")
            .collect();
        let [(_, to)] = relaunches.as_slice() else {
            panic!("{:?}", journal.0);
        };
        assert!(
            items.iter().all(|item| to.contains(&item.id())),
            "{to}: {:?}",
            items.iter().map(|item| item.id()).collect::<Vec<_>>()
        );
    }

    /// Each line a journal was handed: its id and `to`.
    #[derive(Default)]
    struct Recorded(Vec<(String, String)>);

    impl aterm_converge::Journal for Recorded {
        fn before_change(
            &mut self,
            id: &str,
            _from: &str,
            to: &str,
            _revert: Option<&str>,
        ) -> Result<(), String> {
            self.0.push((id.into(), to.into()));
            Ok(())
        }
    }

    #[test]
    fn every_item_is_of_the_family_its_baseline_table_names() {
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
        assert!(
            !seen.contains(&"homebrew") && !seen.contains(&"app"),
            "the starter installs no Homebrew and no app: {seen:?}"
        );
        // The families stay for an owner's own baseline.
        let owned = format!(
            "{text}\n[homebrew]\npkg = {{ url = \"https://example.com/Homebrew.pkg\", sha256 = \
             \"{zero}\", team_id = \"927JGANW46\" }}\nformulae = [\"jq\"]\n\n[[app]]\n\
             name = \"Caffeine\"\nkind = \"dmg-app\"\nurl = \"https://example.com/Caffeine.dmg\"\n\
             sha256 = \"{zero}\"\nteam_id = \"YD6LEYT6WZ\"\nbundle = \"Caffeine.app\"\n\
             version = \"1.1.4\"\n",
            zero = "0".repeat(64)
        );
        let items = expand(
            &crate::baseline::parse(&owned).unwrap(),
            &ctx(&[user("admin", 501)]),
        )
        .unwrap();
        for family in ["homebrew", "app"] {
            assert!(
                items.iter().any(|item| item.family() == family),
                "an owner's [{family}] expands"
            );
        }
    }
}

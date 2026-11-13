// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Signed policy for vendor programs: yanks from the channel's `yanked` list, latched on disk.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::durable;
use super::table::is_vendor;
use super::version::{Version, is_vendor_build};
use crate::manifest::Index;
use crate::sig::{BuildFloor, TrustedIndex};
use crate::store::Layout;

const LATCH_SCHEMA: u32 = 1;
const MAX_LATCH_BYTES: usize = 64 * 1024;

/// How long a fresh index verification keeps the latch current for a targeted vendor door.
pub(crate) const LATCH_CURRENT_FOR_SECS: i64 = 60 * 60;

const LATCH_CLOCK_SLACK_SECS: i64 = 60 * 60;

/// The vendor yanks a signed channel carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// program → yanked versions.
    yanked: BTreeMap<String, BTreeSet<Version>>,
    /// program → yanked legacy (ALab index) build numbers.
    legacy: BTreeMap<String, BTreeSet<u64>>,
    /// ALab-built agent program ([`crate::alab`]) → the signed build floor its channels set
    /// (`min_build_for`, the highest of them): every ALab build below it is revoked, as the
    /// index lane revokes one ([`crate::gate`]). Only ALab-built programs carry one; a vendor
    /// build is ordered by version, which a build floor does not speak.
    floors: BTreeMap<String, u64>,
}

impl Policy {
    /// The vendor entries of every channel of `index`.
    #[must_use]
    pub fn from_index(index: &Index) -> Self {
        let mut policy = Self::from_entries(
            index
                .channels
                .iter()
                .flat_map(|c| c.yanked.iter().map(String::as_str)),
        );
        for agent in crate::alab::ALAB_AGENTS {
            let floor = index
                .channels
                .iter()
                .map(|c| c.min_build_for(agent.program))
                .max()
                .unwrap_or(0);
            policy.set_floor(agent.program, floor);
        }
        policy
    }

    fn set_floor(&mut self, program: &str, floor: u64) {
        if floor > 0 && crate::alab::agent(program).is_some() {
            self.floors.insert(program.to_string(), floor);
        }
    }

    /// The signed build floor of ALab-built `program` (`0`: none).
    #[must_use]
    pub(crate) fn alab_floor(&self, program: &str) -> u64 {
        self.floors.get(program).copied().unwrap_or(0)
    }

    /// The policy of the newest index this machine verified, else the latch, else none.
    #[must_use]
    pub(crate) fn current(layout: &Layout, fresh: Option<&TrustedIndex>) -> Self {
        let latch = YankLatch::read(layout);
        match (fresh, latch) {
            (Some(index), Some(latch)) if !latch.admits(index) => latch.policy,
            (Some(index), _) => Self::from_index(index),
            (None, latch) => latch.map(|l| l.policy).unwrap_or_default(),
        }
    }

    /// Fold any vendor yanks and ALab build floors from `channel` into this policy.
    #[must_use]
    pub(crate) fn with_channel(mut self, channel: Option<&crate::manifest::Channel>) -> Self {
        let Some(ch) = channel else {
            return self;
        };
        for entry in &ch.yanked {
            if let Some((program, rest)) = entry.split_once('@')
                && is_vendor(program)
            {
                self.insert(program, rest);
            }
        }
        for agent in crate::alab::ALAB_AGENTS {
            let floor = self
                .alab_floor(agent.program)
                .max(ch.min_build_for(agent.program));
            self.set_floor(agent.program, floor);
        }
        self
    }

    /// Keep each `"<program>@<version>"` or `"<program>@<build>"` whose program is vendor-direct.
    #[must_use]
    pub(crate) fn from_entries<'a>(entries: impl IntoIterator<Item = &'a str>) -> Self {
        let mut policy = Self::default();
        for entry in entries {
            if let Some((program, rest)) = entry.split_once('@')
                && is_vendor(program)
            {
                policy.insert(program, rest);
            }
        }
        policy
    }

    fn insert(&mut self, program: &str, spelling: &str) {
        if let Some(version) = Version::parse(spelling) {
            self.yanked
                .entry(program.to_string())
                .or_default()
                .insert(version);
        } else if !spelling.starts_with('+')
            && let Ok(build) = spelling.parse::<u64>()
        {
            if let Some(version) = Version::from_build_id(build) {
                self.yanked
                    .entry(program.to_string())
                    .or_default()
                    .insert(version);
            } else if !is_vendor_build(build) {
                self.legacy
                    .entry(program.to_string())
                    .or_default()
                    .insert(build);
            }
        }
    }

    /// Whether `program`'s `version` is yanked.
    #[must_use]
    pub fn is_yanked(&self, program: &str, version: Version) -> bool {
        self.yanked
            .get(program)
            .is_some_and(|set| set.contains(&version))
    }

    /// Whether `program`'s legacy index build `build` is yanked.
    #[must_use]
    pub(crate) fn is_legacy_yanked(&self, program: &str, build: u64) -> bool {
        self.legacy
            .get(program)
            .is_some_and(|set| set.contains(&build))
    }

    /// Every yanked version of `program`, ascending.
    #[must_use]
    pub(crate) fn yanked_of(&self, program: &str) -> Vec<Version> {
        self.yanked
            .get(program)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    fn spellings(&self) -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (program, set) in &self.yanked {
            out.entry(program.clone())
                .or_default()
                .extend(set.iter().map(Version::to_string));
        }
        for (program, set) in &self.legacy {
            out.entry(program.clone())
                .or_default()
                .extend(set.iter().map(|b| b.to_string()));
        }
        out
    }
}

/// The vendor yanks of the newest freshly verified index (`<prefix>/vendor-yanks.toml`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct YankLatch {
    /// The index build these yanks came from.
    pub index_build: u64,
    /// The roster generation that index was authorized by.
    pub roster_seq: Option<u64>,
    /// Its vendor yanks.
    pub policy: Policy,
    /// When a fresh verification last wrote or confirmed it (Unix seconds).
    pub checked_at: i64,
}

#[derive(Serialize, Deserialize)]
struct LatchFile {
    schema: u32,
    index_build: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    roster_seq: Option<u64>,
    #[serde(default)]
    checked_at: i64,
    #[serde(default)]
    yanks: BTreeMap<String, Vec<String>>,
    /// [`Policy::alab_floor`]s. A reader that predates them ignores the table.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    floors: BTreeMap<String, u64>,
}

/// `<prefix>/vendor-yanks.toml`.
#[must_use]
pub(crate) fn latch_path(layout: &Layout) -> PathBuf {
    layout.prefix.join("vendor-yanks.toml")
}

impl YankLatch {
    /// The latch on disk, or `None` when absent or unreadable.
    #[must_use]
    pub(crate) fn read(layout: &Layout) -> Option<Self> {
        let file: LatchFile = durable::read_toml(&latch_path(layout), MAX_LATCH_BYTES)?;
        if file.schema != LATCH_SCHEMA {
            return None;
        }
        let mut policy = Policy::default();
        for (program, spellings) in &file.yanks {
            if is_vendor(program) {
                for spelling in spellings {
                    policy.insert(program, spelling);
                }
            }
        }
        for (program, floor) in &file.floors {
            policy.set_floor(program, *floor);
        }
        Some(Self {
            index_build: file.index_build,
            roster_seq: file.roster_seq,
            policy,
            checked_at: file.checked_at,
        })
    }

    fn admits(&self, index: &TrustedIndex) -> bool {
        let (index_build, generation, _) = rank(index);
        self.admits_at(generation, index_build)
    }

    const fn admits_at(&self, generation: u64, index_build: u64) -> bool {
        match self.roster_seq {
            Some(roster_seq) => BuildFloor {
                index_build: self.index_build,
                roster_seq,
            }
            .admits(generation, index_build),
            None => index_build >= self.index_build,
        }
    }

    /// Whether a fresh verification wrote or confirmed the latch within [`LATCH_CURRENT_FOR_SECS`] of `now`.
    #[must_use]
    pub(crate) fn is_current(&self, now: i64) -> bool {
        let age = now.saturating_sub(self.checked_at);
        self.checked_at > 0 && (-LATCH_CLOCK_SLACK_SECS..=LATCH_CURRENT_FOR_SECS).contains(&age)
    }

    /// Record every channel's yanks from `index`, verified at `checked_at`, unless the latch
    /// holds a newer index.
    ///
    /// # Errors
    /// The latch could not be written.
    pub(crate) fn update_from_index(
        layout: &Layout,
        index: &TrustedIndex,
        checked_at: i64,
    ) -> io::Result<bool> {
        Self::update(layout, rank(index), Policy::from_index(index), checked_at)
    }

    /// Carry yanks forward from cached `index`, unless the latch holds a newer index, while
    /// preserving the last live verification stamp.
    ///
    /// # Errors
    /// The latch could not be written.
    pub(crate) fn carry_from_cached_index(
        layout: &Layout,
        index: &TrustedIndex,
    ) -> io::Result<bool> {
        let stamp = Self::read(layout).map_or(0, |l| l.checked_at);
        Self::update(layout, rank(index), Policy::from_index(index), stamp)
    }

    fn update(
        layout: &Layout,
        (index_build, generation, roster_seq): (u64, u64, Option<u64>),
        policy: Policy,
        checked_at: i64,
    ) -> io::Result<bool> {
        let next = Self {
            index_build,
            roster_seq,
            policy,
            checked_at,
        };
        if let Some(stored) = Self::read(layout)
            && (!stored.admits_at(generation, index_build) || stored == next)
        {
            return Ok(false);
        }
        let file = LatchFile {
            schema: LATCH_SCHEMA,
            index_build,
            roster_seq,
            checked_at,
            yanks: next.policy.spellings(),
            floors: next.policy.floors.clone(),
        };
        let bytes = durable::to_toml(&file, MAX_LATCH_BYTES)?;
        durable::write_durable(&latch_path(layout), &bytes)?;
        Ok(true)
    }
}

fn rank(index: &TrustedIndex) -> (u64, u64, Option<u64>) {
    (
        index.index_build,
        index.authorizing_seq(),
        Some(index.roster_seq()),
    )
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;

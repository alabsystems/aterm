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
#[cfg(test)]
use crate::manifest::Channel;
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
    /// The vendor entries of `channel.yanked`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_channel(channel: &Channel) -> Self {
        Self::from_entries(channel.yanked.iter().map(String::as_str))
    }

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
        } else if let Ok(build) = spelling.parse::<u64>() {
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

    /// Record `channel`'s vendor yanks from `index_build`, verified at `checked_at`.
    ///
    /// # Errors
    /// The latch could not be written.
    #[cfg(test)]
    pub(crate) fn update_from_fresh_index(
        layout: &Layout,
        index_build: u64,
        channel: &Channel,
        checked_at: i64,
    ) -> io::Result<bool> {
        Self::update(
            layout,
            (index_build, 0, None),
            Policy::from_channel(channel),
            checked_at,
        )
    }

    /// Record every channel's yanks from `index`, verified at `checked_at`.
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

    /// Carry yanks forward from cached `index` while preserving the last live verification stamp.
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
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn channel(yanked: &[&str]) -> Channel {
        Channel {
            name: "stable".into(),
            channel_build: 1,
            min_build: 0,
            min_build_by_program: BTreeMap::new(),
            yanked: yanked.iter().map(|s| (*s).to_string()).collect(),
            pin: BTreeMap::new(),
            pin_by_target: BTreeMap::new(),
        }
    }

    fn layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-vendor-policy-{label}-"))
    }

    fn index(index_build: u64, yanked: &[&str]) -> TrustedIndex {
        index_at(crate::sig::testkit::SEQ, index_build, yanked)
    }

    fn index_at(generation: u64, index_build: u64, yanked: &[&str]) -> TrustedIndex {
        let yanked: Vec<String> = yanked.iter().map(|y| format!("\"{y}\"")).collect();
        let body = format!(
            "schema = 2\nindex_build = {index_build}\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
             machine_id = \"{}\"\nroster_seq = {generation}\n\
             [[channels]]\nname = \"stable\"\nyanked = [{}]\n",
            crate::sig::testkit::MACHINE_ID,
            yanked.join(", ")
        );
        crate::sig::testkit::trusted_index_at(generation, body.into_bytes())
    }

    #[test]
    fn a_newer_generation_supersedes_the_latch_whatever_its_index_build() {
        let l = layout("rebase");
        let poisoned = index_at(3, i64::MAX.unsigned_abs(), &[]);
        YankLatch::update_from_index(&l, &poisoned, 100).unwrap();
        let rescue = index_at(4, 101, &["claude@2.1.300"]);
        assert!(
            Policy::current(&l, Some(&rescue)).is_yanked("claude", v("2.1.300")),
            "the newer generation's yank was ignored for the latched one"
        );
        assert!(YankLatch::update_from_index(&l, &rescue, 200).unwrap());
        assert!(Policy::current(&l, None).is_yanked("claude", v("2.1.300")));
        assert!(
            Policy::current(&l, Some(&index_at(4, 100, &[]))).is_yanked("claude", v("2.1.300")),
            "an older build of the new generation lifted the rescue's yank"
        );
    }

    #[test]
    fn an_older_fresh_index_never_lifts_a_latched_yank() {
        let l = layout("replay");
        YankLatch::update_from_index(&l, &index(45, &["claude@2.1.281"]), 100).unwrap();
        let replayed = Policy::current(&l, Some(&index(44, &[])));
        assert!(
            replayed.is_yanked("claude", v("2.1.281")),
            "index 44 lifted 45's yank"
        );
        let newer = Policy::current(&l, Some(&index(46, &[])));
        assert!(
            !newer.is_yanked("claude", v("2.1.281")),
            "a newer index may lift it"
        );
        let same = Policy::current(&l, Some(&index(45, &["claude@2.1.281"])));
        assert!(same.is_yanked("claude", v("2.1.281")));
    }

    #[test]
    fn a_cached_verification_carries_yanks_but_not_currency() {
        let l = layout("cached");
        YankLatch::update_from_index(&l, &index(44, &[]), 1_000).unwrap();
        YankLatch::carry_from_cached_index(&l, &index(45, &["codex@0.156.1"])).unwrap();
        let latch = YankLatch::read(&l).unwrap();
        assert_eq!(latch.index_build, 45);
        assert!(latch.policy.is_yanked("codex", v("0.156.1")));
        assert_eq!(latch.checked_at, 1_000, "cached bytes restamped the latch");
        assert!(!latch.is_current(1_000 + LATCH_CURRENT_FOR_SECS + 1));
        let fresh = layout("cached-empty");
        YankLatch::carry_from_cached_index(&fresh, &index(44, &[])).unwrap();
        assert!(!YankLatch::read(&fresh).unwrap().is_current(0));
    }

    #[test]
    fn a_vendor_programs_entries_name_versions_or_builds() {
        let ch = channel(&[
            "claude@2.1.279",
            "codex@0.155.1",
            "trust@4790",
            "claude@2026091901",
            "claude@2.1.08",
            "ay@1.2.3",
            "garbage",
            "claude@",
            "claude@-1",
            "claude@3000000000000000000",
        ]);
        let p = Policy::from_channel(&ch);
        assert!(p.is_yanked("claude", v("2.1.279")));
        assert!(p.is_yanked("codex", v("0.155.1")));
        assert!(!p.is_yanked("claude", v("2.1.280")));
        assert!(!p.is_yanked("ay", v("1.2.3")), "ay is not vendor-direct");
        assert_eq!(p.yanked_of("claude"), vec![v("2.1.279")]);
        assert_eq!(p.yanked_of("trust"), Vec::<Version>::new());
        assert!(p.is_legacy_yanked("claude", 2_026_091_901));
        assert!(!p.is_legacy_yanked("claude", 2_026_091_902));
        assert!(
            !p.is_legacy_yanked("trust", 4790),
            "trust is not vendor-direct"
        );
    }

    #[test]
    fn a_vendor_build_id_entry_yanks_its_version() {
        let id = v("2.1.285").build_id();
        let entry = format!("claude@{id}");
        let ch = channel(&[entry.as_str()]);
        assert!(crate::gate::is_yanked(&ch, "claude", id));
        let p = Policy::from_channel(&ch);
        assert!(p.is_yanked("claude", v("2.1.285")));
        assert!(!p.is_legacy_yanked("claude", id));
        let padded = format!("claude@0{id}");
        assert!(Policy::from_entries([padded.as_str()]).is_yanked("claude", v("2.1.285")));
    }

    #[test]
    fn a_revision_form_entry_yanks_its_version_by_text_or_build() {
        let head = v("1.4.9-R6297.1");
        for spelling in [head.to_string(), head.build_id().to_string()] {
            let mut p = Policy::default();
            p.insert("muse", &spelling);
            assert!(p.is_yanked("muse", head), "{spelling}");
            assert!(!p.is_yanked("muse", v("1.4.9-R6297")), "{spelling}");
            assert!(!p.is_legacy_yanked("muse", head.build_id()), "{spelling}");
        }
    }

    #[test]
    fn gate_is_yanked_ignores_version_entries() {
        let ch = channel(&["claude@2.1.279", "trust@4790"]);
        assert!(crate::gate::is_yanked(&ch, "trust", 4790));
        assert!(!crate::gate::is_yanked(
            &ch,
            "claude",
            v("2.1.279").build_id()
        ));
        for build in [0, 2, 1, 279, 21_279, 2_001_279] {
            assert!(!crate::gate::is_yanked(&ch, "claude", build), "{build}");
        }
    }

    #[test]
    fn the_latch_advances_with_fresh_indexes_and_never_by_staleness() {
        let l = layout("advance");
        assert_eq!(YankLatch::read(&l), None);
        let first = channel(&[
            "claude@2.1.279",
            "trust@4790",
            "claude@2026091901",
            "codex@1000000000156000001",
        ]);
        assert!(YankLatch::update_from_fresh_index(&l, 44, &first, 100).unwrap());
        let got = YankLatch::read(&l).unwrap();
        assert_eq!(got.index_build, 44);
        assert_eq!(got.checked_at, 100);
        assert_eq!(
            got.policy,
            Policy::from_channel(&first),
            "every spelling round-trips"
        );
        assert!(got.policy.is_yanked("claude", v("2.1.279")));
        assert!(got.policy.is_yanked("codex", v("0.156.1")));
        assert!(got.policy.is_legacy_yanked("claude", 2_026_091_901));
        assert!(!YankLatch::update_from_fresh_index(&l, 44, &first, 100).unwrap());
        assert!(YankLatch::update_from_fresh_index(&l, 44, &first, 200).unwrap());
        let confirmed = YankLatch::read(&l).unwrap();
        assert_eq!((confirmed.index_build, confirmed.checked_at), (44, 200));
        assert_eq!(confirmed.policy, got.policy);
        let older = channel(&[]);
        assert!(!YankLatch::update_from_fresh_index(&l, 43, &older, 300).unwrap());
        assert_eq!(YankLatch::read(&l).unwrap().checked_at, 200);
        assert!(
            YankLatch::read(&l)
                .unwrap()
                .policy
                .is_yanked("claude", v("2.1.279"))
        );
        assert!(YankLatch::update_from_fresh_index(&l, 45, &older, 300).unwrap());
        let lifted = YankLatch::read(&l).unwrap();
        assert_eq!(lifted.index_build, 45);
        assert!(!lifted.policy.is_yanked("claude", v("2.1.279")));
        let again = channel(&["codex@0.156.0"]);
        assert!(YankLatch::update_from_fresh_index(&l, 45, &again, 300).unwrap());
        assert!(
            YankLatch::read(&l)
                .unwrap()
                .policy
                .is_yanked("codex", v("0.156.0"))
        );
    }

    #[test]
    fn a_latch_is_current_for_an_hour_after_its_verification() {
        let at = |checked_at: i64| YankLatch {
            index_build: 44,
            roster_seq: None,
            policy: Policy::default(),
            checked_at,
        };
        let now = 1_790_035_200;
        assert!(at(now).is_current(now));
        assert!(at(now - LATCH_CURRENT_FOR_SECS).is_current(now));
        assert!(!at(now - LATCH_CURRENT_FOR_SECS - 1).is_current(now));
        assert!(
            !at(0).is_current(now),
            "a latch from before the stamp was kept"
        );
        assert!(at(now + LATCH_CLOCK_SLACK_SECS).is_current(now));
        assert!(!at(now + LATCH_CLOCK_SLACK_SECS + 1).is_current(now));
        let l = layout("unstamped");
        std::fs::write(
            latch_path(&l),
            "schema = 1\nindex_build = 9\n[yanks]\nclaude = [\"2.1.279\"]\n",
        )
        .unwrap();
        let old = YankLatch::read(&l).unwrap();
        assert_eq!(old.checked_at, 0);
        assert!(!old.is_current(now));
        assert!(old.policy.is_yanked("claude", v("2.1.279")));
    }

    #[test]
    fn a_malformed_latch_reads_as_absent_and_a_bad_entry_drops_alone() {
        let l = layout("malformed");
        std::fs::write(latch_path(&l), "this is not toml {{{").unwrap();
        assert_eq!(YankLatch::read(&l), None);
        std::fs::write(latch_path(&l), "schema = 2\nindex_build = 9\n").unwrap();
        assert_eq!(YankLatch::read(&l), None, "an unknown schema is not read");
        std::fs::write(
            latch_path(&l),
            "schema = 1\nindex_build = 9\n[yanks]\nclaude = [\"2.1.279\", \"2.1.08\"]\n",
        )
        .unwrap();
        let got = YankLatch::read(&l).unwrap();
        assert_eq!(got.policy.yanked_of("claude"), vec![v("2.1.279")]);
        std::fs::write(
            latch_path(&l),
            "schema = 1\nindex_build = 9\n[yanks]\nclaude = [\"2.1.279\", \"2026091901\", \"x\"]\n",
        )
        .unwrap();
        let got = YankLatch::read(&l).unwrap();
        assert!(got.policy.is_yanked("claude", v("2.1.279")));
        assert!(got.policy.is_legacy_yanked("claude", 2_026_091_901));
        std::fs::write(latch_path(&l), "garbage").unwrap();
        assert!(YankLatch::update_from_fresh_index(&l, 1, &channel(&[]), 1).unwrap());
        assert_eq!(YankLatch::read(&l).unwrap().index_build, 1);
    }
}

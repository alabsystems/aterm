// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Signed policy for vendor programs (design §1.4): yanks only, from the channel's
//! existing `yanked` list, latched on disk so staleness can never un-apply one.
//!
//! A vendor program's entry may name its version (`"claude@2.1.285"`) or a build number
//! as `gate::is_yanked` spells it: a vendor build id (`"claude@1000002000001000285"`)
//! names that same version, and an ALab build number names a legacy index build. A
//! build-number entry means the same build to both readers; the version spelling is this
//! reader's alone, so older clients skip it.

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
use crate::store::Layout;

/// The latch file's schema.
const LATCH_SCHEMA: u32 = 1;

/// Bound on a latch read.
const MAX_LATCH_BYTES: usize = 64 * 1024;

/// How long a fresh index verification keeps the latch current for a targeted vendor
/// door (seconds): past it, or with no latch, the door verifies the index first.
pub(crate) const LATCH_CURRENT_FOR_SECS: i64 = 60 * 60;

/// How far ahead of the clock a latch may be stamped and still be current (seconds): one
/// stamped further ahead was written by a clock that was wrong.
const LATCH_CLOCK_SLACK_SECS: i64 = 60 * 60;

/// The vendor yanks a signed channel carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Policy {
    /// program → yanked versions.
    yanked: BTreeMap<String, BTreeSet<Version>>,
    /// program → yanked legacy (ALab index) build numbers.
    legacy: BTreeMap<String, BTreeSet<u64>>,
}

impl Policy {
    /// The vendor entries of `channel.yanked`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_channel(channel: &Channel) -> Self {
        Self::from_entries(channel.yanked.iter().map(String::as_str))
    }

    /// The vendor entries of every channel of `index`: a vendor program follows its
    /// vendor, not a channel, so a yank in any channel binds it.
    #[must_use]
    pub(crate) fn from_index(index: &Index) -> Self {
        Self::from_entries(
            index
                .channels
                .iter()
                .flat_map(|c| c.yanked.iter().map(String::as_str)),
        )
    }

    /// The policy the lane reads: the yanks of the NEWEST index this machine verified —
    /// `fresh` (verified this pass) unless the latch holds a newer one — else the latch,
    /// else none. A replayed older index, still signed and inside its window, never lifts
    /// a yank a newer one latched (design §1.4).
    #[must_use]
    pub(crate) fn current(layout: &Layout, fresh: Option<&Index>) -> Self {
        let latch = YankLatch::read(layout);
        match (fresh, latch) {
            (Some(index), Some(latch)) if latch.index_build > index.index_build => latch.policy,
            (Some(index), _) => Self::from_index(index),
            (None, latch) => latch.map(|l| l.policy).unwrap_or_default(),
        }
    }

    /// Keep each `"<program>@<x.y.z>"` or `"<program>@<build>"` whose program is
    /// vendor-direct; every other entry (an ALab program's, a typo) is ignored.
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

    /// One vendor program's entry. A build number parses as `gate::is_yanked` parses it,
    /// so the two readers never disagree on which build an entry names.
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
    pub(crate) fn is_yanked(&self, program: &str, version: Version) -> bool {
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

    /// program → the entries as the latch stores them: versions, then legacy builds.
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
                .extend(set.iter().map(|b| crate::dec_u64(*b)));
        }
        out
    }
}

/// The vendor yanks of the newest FRESHLY verified index (`<prefix>/vendor-yanks.toml`).
/// Read when no index verifies this pass; replaced only by an index at least as new.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct YankLatch {
    /// The index build these yanks came from.
    pub index_build: u64,
    /// Its vendor yanks.
    pub policy: Policy,
    /// When a fresh verification last wrote or confirmed it (Unix seconds; `0` for a
    /// latch written before this was kept, which is never current).
    pub checked_at: i64,
}

#[derive(Serialize, Deserialize)]
struct LatchFile {
    schema: u32,
    index_build: u64,
    #[serde(default)]
    checked_at: i64,
    #[serde(default)]
    yanks: BTreeMap<String, Vec<String>>,
}

/// `<prefix>/vendor-yanks.toml`.
#[must_use]
pub(crate) fn latch_path(layout: &Layout) -> PathBuf {
    layout.prefix.join("vendor-yanks.toml")
}

impl YankLatch {
    /// The latch on disk. `None` when absent or unreadable; an entry that does not parse
    /// is dropped alone, so one bad line cannot lift every other yank.
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
        Some(Self {
            index_build: file.index_build,
            policy,
            checked_at: file.checked_at,
        })
    }

    /// Whether a fresh verification wrote or confirmed the latch at most
    /// [`LATCH_CURRENT_FOR_SECS`] before `now` (and not more than an hour after it): a
    /// targeted door then decides on it without verifying the index again.
    #[must_use]
    pub(crate) fn is_current(&self, now: i64) -> bool {
        let age = now.saturating_sub(self.checked_at);
        self.checked_at > 0 && (-LATCH_CLOCK_SLACK_SECS..=LATCH_CURRENT_FOR_SECS).contains(&age)
    }

    /// Record `channel`'s vendor yanks from a FRESHLY verified index `index_build`,
    /// verified at `checked_at`, unless the latch already holds a newer index. The caller
    /// holds the Floor's lock, which serializes every index admission. Returns whether the
    /// file was rewritten (an identical latch is not; the same yanks confirmed later are
    /// — that is what keeps it current).
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
            index_build,
            Policy::from_channel(channel),
            checked_at,
        )
    }

    /// [`Self::update_from_fresh_index`] with every channel's yanks ([`Policy::from_index`]):
    /// what each fresh verification records.
    ///
    /// # Errors
    /// The latch could not be written.
    pub(crate) fn update_from_index(
        layout: &Layout,
        index: &Index,
        checked_at: i64,
    ) -> io::Result<bool> {
        Self::update(
            layout,
            index.index_build,
            Policy::from_index(index),
            checked_at,
        )
    }

    /// [`Self::update_from_index`] for an index verified from CACHED bytes (the source was
    /// not reached): its yanks are carried forward, but the stamp stays what the last
    /// live verification left, so a targeted door still verifies live when it is due.
    ///
    /// # Errors
    /// The latch could not be written.
    pub(crate) fn carry_from_cached_index(layout: &Layout, index: &Index) -> io::Result<bool> {
        let stamp = Self::read(layout).map_or(0, |l| l.checked_at);
        Self::update(layout, index.index_build, Policy::from_index(index), stamp)
    }

    fn update(
        layout: &Layout,
        index_build: u64,
        policy: Policy,
        checked_at: i64,
    ) -> io::Result<bool> {
        let next = Self {
            index_build,
            policy,
            checked_at,
        };
        if let Some(stored) = Self::read(layout)
            && (index_build < stored.index_build || stored == next)
        {
            return Ok(false);
        }
        let file = LatchFile {
            schema: LATCH_SCHEMA,
            index_build,
            checked_at,
            yanks: next.policy.spellings(),
        };
        let bytes = durable::to_toml(&file, MAX_LATCH_BYTES)?;
        durable::write_durable(&latch_path(layout), &bytes)?;
        Ok(true)
    }
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
            meta: BTreeMap::new(),
        }
    }

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!(
            "atpkg-vendor-policy-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Layout { prefix: p }
    }

    fn index(index_build: u64, yanked: &[&str]) -> Index {
        Index {
            schema: 2,
            index_build,
            generated_at: String::new(),
            valid_until: "2099-01-01T00:00:00Z".into(),
            machine_id: None,
            roster_seq: None,
            programs: BTreeMap::new(),
            channels: vec![channel(yanked)],
        }
    }

    /// A replayed OLDER index (signed, in its window) never lifts a yank the latch took
    /// from a newer one; a newer or equal fresh index is what the lane reads.
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

    /// Cached bytes carry their yanks forward but never vouch that the latch is current.
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
        // With no latch at all, cached bytes leave it never-current.
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
            "claude@2000000000000000000",
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

    /// A publisher who copies a vendor build's directory name from the store yanks that
    /// version: the lane and `gate::is_yanked` read the entry alike.
    #[test]
    fn a_vendor_build_id_entry_yanks_its_version() {
        let id = v("2.1.285").build_id();
        let entry = format!("claude@{id}");
        let ch = channel(&[entry.as_str()]);
        assert!(crate::gate::is_yanked(&ch, "claude", id));
        let p = Policy::from_channel(&ch);
        assert!(p.is_yanked("claude", v("2.1.285")));
        assert!(!p.is_legacy_yanked("claude", id));
        // gate's parse, so a spelling gate reads the same way is the same yank.
        let padded = format!("claude@0{id}");
        assert!(Policy::from_entries([padded.as_str()]).is_yanked("claude", v("2.1.285")));
    }

    /// `gate::is_yanked` keeps its build-number meaning for ALab programs and never reads a
    /// version spelling — not even as the version's own build id.
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
        // The same index at the same moment rewrites nothing; confirmed later, only the
        // time moves.
        assert!(!YankLatch::update_from_fresh_index(&l, 44, &first, 100).unwrap());
        assert!(YankLatch::update_from_fresh_index(&l, 44, &first, 200).unwrap());
        let confirmed = YankLatch::read(&l).unwrap();
        assert_eq!((confirmed.index_build, confirmed.checked_at), (44, 200));
        assert_eq!(confirmed.policy, got.policy);
        // An OLDER index cannot lift the yank, nor confirm the latch.
        let older = channel(&[]);
        assert!(!YankLatch::update_from_fresh_index(&l, 43, &older, 300).unwrap());
        assert_eq!(YankLatch::read(&l).unwrap().checked_at, 200);
        assert!(
            YankLatch::read(&l)
                .unwrap()
                .policy
                .is_yanked("claude", v("2.1.279"))
        );
        // A NEWER index can.
        assert!(YankLatch::update_from_fresh_index(&l, 45, &older, 300).unwrap());
        let lifted = YankLatch::read(&l).unwrap();
        assert_eq!(lifted.index_build, 45);
        assert!(!lifted.policy.is_yanked("claude", v("2.1.279")));
        // An equal build with different content is a fresh verification: it wins.
        let again = channel(&["codex@0.156.0"]);
        assert!(YankLatch::update_from_fresh_index(&l, 45, &again, 300).unwrap());
        assert!(
            YankLatch::read(&l)
                .unwrap()
                .policy
                .is_yanked("codex", v("0.156.0"))
        );
    }

    /// A latch is current for an hour after the fresh verification that wrote or last
    /// confirmed it; one never stamped, or stamped more than an hour ahead of the clock,
    /// is not.
    #[test]
    fn a_latch_is_current_for_an_hour_after_its_verification() {
        let at = |checked_at: i64| YankLatch {
            index_build: 44,
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
        // A latch written before `checked_at` was kept reads as never checked.
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
        // An unreadable latch is replaced by the next fresh index, whatever its build.
        std::fs::write(latch_path(&l), "garbage").unwrap();
        assert!(YankLatch::update_from_fresh_index(&l, 1, &channel(&[]), 1).unwrap());
        assert_eq!(YankLatch::read(&l).unwrap().index_build, 1);
    }
}

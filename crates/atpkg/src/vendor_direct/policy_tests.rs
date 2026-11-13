// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::manifest::Channel;

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
    let entries = [
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
    ];
    let p = Policy::from_index(&index(1, &entries));
    assert_eq!(p, Policy::from_entries(entries));
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
    let p = Policy::from_index(&index(1, &[entry.as_str()]));
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
    let first = index(
        44,
        &[
            "claude@2.1.279",
            "trust@4790",
            "claude@2026091901",
            "codex@1000000000156000001",
        ],
    );
    assert!(YankLatch::update_from_index(&l, &first, 100).unwrap());
    let got = YankLatch::read(&l).unwrap();
    assert_eq!(got.index_build, 44);
    assert_eq!(got.roster_seq, Some(crate::sig::testkit::SEQ));
    assert_eq!(got.checked_at, 100);
    assert_eq!(
        got.policy,
        Policy::from_index(&first),
        "every spelling round-trips"
    );
    assert!(got.policy.is_yanked("claude", v("2.1.279")));
    assert!(got.policy.is_yanked("codex", v("0.156.1")));
    assert!(got.policy.is_legacy_yanked("claude", 2_026_091_901));
    assert!(!YankLatch::update_from_index(&l, &first, 100).unwrap());
    assert!(YankLatch::update_from_index(&l, &first, 200).unwrap());
    let confirmed = YankLatch::read(&l).unwrap();
    assert_eq!((confirmed.index_build, confirmed.checked_at), (44, 200));
    assert_eq!(confirmed.policy, got.policy);
    let older = index(43, &[]);
    assert!(!YankLatch::update_from_index(&l, &older, 300).unwrap());
    assert_eq!(YankLatch::read(&l).unwrap().checked_at, 200);
    assert!(
        YankLatch::read(&l)
            .unwrap()
            .policy
            .is_yanked("claude", v("2.1.279"))
    );
    let newer = index(45, &[]);
    assert!(YankLatch::update_from_index(&l, &newer, 300).unwrap());
    let lifted = YankLatch::read(&l).unwrap();
    assert_eq!(lifted.index_build, 45);
    assert!(!lifted.policy.is_yanked("claude", v("2.1.279")));
    let again = index(45, &["codex@0.156.0"]);
    assert!(YankLatch::update_from_index(&l, &again, 300).unwrap());
    assert!(
        YankLatch::read(&l)
            .unwrap()
            .policy
            .is_yanked("codex", v("0.156.0"))
    );

    let pre_gen = layout("pre-gen");
    std::fs::write(
        latch_path(&pre_gen),
        "schema = 1\nindex_build = 44\nchecked_at = 50\n[yanks]\nclaude = [\"2.1.279\"]\n",
    )
    .unwrap();
    assert_eq!(YankLatch::read(&pre_gen).unwrap().roster_seq, None);
    assert!(!YankLatch::update_from_index(&pre_gen, &older, 60).unwrap());
    assert!(Policy::current(&pre_gen, Some(&older)).is_yanked("claude", v("2.1.279")));
    assert!(YankLatch::update_from_index(&pre_gen, &newer, 70).unwrap());
    let upgraded = YankLatch::read(&pre_gen).unwrap();
    assert_eq!(upgraded.roster_seq, Some(crate::sig::testkit::SEQ));
    assert_eq!(upgraded.index_build, 45);
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
    assert!(YankLatch::update_from_index(&l, &index(1, &[]), 1).unwrap());
    assert_eq!(YankLatch::read(&l).unwrap().index_build, 1);
}

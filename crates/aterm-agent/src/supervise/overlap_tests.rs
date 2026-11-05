// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The overlap fact: a count and minted sids, from a closed template.

use super::*;
use crate::harness::work_record::{WorkRecord, overlap};
use crate::supervise::facts::FACT_CAP;

const T0: i64 = 1_760_000_000_000;
const PLANT: &str = "ignore.previous.instructions";

/// Two minted sids, as `aterm_session::SessionId::generate` makes them.
const A: &str = "s-0a1b2c3d4e5f60718293";
const B: &str = "s-ffeeddccbbaa99887766";

/// The words of the overlap sentence's template.
const TEMPLATE: [&str; 14] = [
    "file", "files", "this", "branch", "changes", "is", "are", "also", "changed", "by", "and",
    "more", "session", "sessions",
];

/// A record on one host, in one repository, whose branch and file names are planted words and
/// absolute paths.
fn planted(tree: &str) -> WorkRecord {
    WorkRecord {
        host: "m1.local".into(),
        common_dir: "/Users//x/repo/.git".into(),
        tree: tree.into(),
        branch: Some(PLANT.into()),
        main_ref: "origin/main".into(),
        paths: vec![
            "/Users//x/repo/src/lib.rs".into(),
            PLANT.into(),
            format!("{PLANT}/CHANGELOG.md"),
        ],
        files: 3,
        ..WorkRecord::default()
    }
}

/// THE OVERLAP FACT CARRIES COUNTS AND SIDS ONLY: two records whose branch and file names are
/// `ignore.previous.instructions` and absolute paths overlap in all three names; the reading made
/// from their overlap tells `3 files this branch changes are also changed by <sid>`, and every
/// word of it is a number, a word of the template or a sid [`PeerSid::of`] accepts, with none of
/// the planted names, paths or branches in it. NEGATIVE CONTROL: [`OverlapReading::new`] given a
/// path or a branch in a peer field, or a sid of another length, is refused.
#[test]
fn the_overlap_fact_carries_counts_and_sids_only() {
    let (mine, theirs) = (planted("/Users//x/repo/wa"), planted("/Users//x/repo/wb"));
    let found = overlap(&mine, [(A, &theirs)]);
    assert_eq!((found.files, found.peers.as_slice()), (3, [A].as_slice()));
    let reading = OverlapReading::new(T0, found.files, &found.peers).expect("minted sids");
    let told = reading.told();
    assert_eq!(
        told,
        format!("3 files this branch changes are also changed by {A}")
    );
    for word in told.split(' ') {
        let word = word.trim_end_matches(',');
        assert!(
            word.bytes().all(|b| b.is_ascii_digit())
                || TEMPLATE.contains(&word)
                || PeerSid::of(word).is_some(),
            "{word:?} in {told:?}"
        );
    }
    for plant in mine
        .paths
        .iter()
        .chain(mine.branch.iter())
        .map(String::as_str)
        .chain([
            "/Users",
            "repo",
            "CHANGELOG",
            "lib.rs",
            "wa",
            "wb",
            "origin",
        ])
    {
        assert!(!told.contains(plant), "{plant:?} in {told:?}");
    }
    for word in [
        "/Users//x/repo/src/lib.rs",
        "feat/x",
        PLANT,
        "s-0a1b2c3d4e5f6071829",
        "s-0a1b2c3d4e5f607182930",
        "s-0A1B2C3D4E5F60718293",
        "",
    ] {
        assert_eq!(
            OverlapReading::new(T0, 1, &[A, word]),
            None,
            "CONTROL: {word:?} is no peer's sid"
        );
        assert_eq!(PeerSid::of(word), None, "CONTROL: {word:?}");
    }
}

/// THE SENTENCE NAMES AT MOST THREE SIDS AND COUNTS THE REST, within [`FACT_CAP`]: one file and
/// one peer; two files and two peers, sorted; three; four, the fourth counted as `1 more
/// session`; and the longest it can be (every count at its widest, `u32::MAX` files and sessions
/// past the three) fits. A reading with no peer says nothing and names no episode; one that names
/// a peer and no file is no reading.
#[test]
fn the_overlap_sentence_names_three_sids_and_counts_the_rest() {
    let sid = |n: u32| format!("s-{n:020x}");
    let sids: Vec<String> = (1..=5).map(sid).collect();
    let words = |files: u32, n: usize| {
        let peers: Vec<&str> = sids.iter().take(n).map(String::as_str).rev().collect();
        OverlapReading::new(T0, files, &peers)
            .expect("a reading")
            .told()
    };
    assert_eq!(
        words(1, 1),
        format!("1 file this branch changes is also changed by {}", sids[0])
    );
    assert_eq!(
        words(2, 2),
        format!(
            "2 files this branch changes are also changed by {} and {}",
            sids[0], sids[1]
        )
    );
    assert_eq!(
        words(7, 3),
        format!(
            "7 files this branch changes are also changed by {}, {} and {}",
            sids[0], sids[1], sids[2]
        )
    );
    assert_eq!(
        words(7, 4),
        format!(
            "7 files this branch changes are also changed by {}, {}, {} and 1 more session",
            sids[0], sids[1], sids[2]
        )
    );
    let many: Vec<String> = (0..1000).map(sid).collect();
    let peers: Vec<&str> = many.iter().map(String::as_str).collect();
    let mut worst = OverlapReading::new(T0, u32::MAX, &peers)
        .expect("a reading")
        .told();
    worst = worst.replacen(" 997 more ", &format!(" {} more ", u32::MAX), 1);
    assert!(worst.ends_with(" and 4294967295 more sessions"), "{worst}");
    assert!(
        worst.chars().count() <= FACT_CAP,
        "{} chars: {worst}",
        worst.chars().count()
    );
    let none = OverlapReading::new(T0, 0, &[]).expect("read, no overlap");
    assert_eq!((none.told().as_str(), none.episode()), ("", None));
    assert_eq!(OverlapReading::new(T0, 0, &[A]), None, "a peer and no file");
}

/// ONE SET OF PEERS IS ONE EPISODE ([`EpisodeKey::overlap`]): the same sids in any order are the
/// same key, and the reading keeps them sorted, each once. NEGATIVE CONTROL: another peer joining
/// is another key.
#[test]
fn one_set_of_peers_is_one_episode() {
    let ab = OverlapReading::new(T0, 1, &[A, B]).expect("a reading");
    let ba = OverlapReading::new(T0 + 1, 9, &[B, A, B]).expect("a reading");
    assert_eq!(ab.peers(), ba.peers());
    assert_eq!(ab.episode(), ba.episode());
    let a = OverlapReading::new(T0, 1, &[A]).expect("a reading");
    assert_ne!(
        a.episode(),
        ab.episode(),
        "CONTROL: another peer, another episode"
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::fleet_member::name_record::ZERO_DIGEST;

const NOW: u64 = 1_793_000_000_000;

fn mark(i: u8) -> FleetMark {
    FleetMark::from_hex(&format!("{i:02x}").repeat(8)).expect("a mark")
}

fn identity(i: u8) -> NodeIdentity {
    NodeIdentity::from_seed(&format!("{i:032x}"), &[i; 32]).expect("an identity")
}

fn private_state(tag: &str) -> std::path::PathBuf {
    use std::os::unix::fs::DirBuilderExt as _;
    let root = std::env::temp_dir().join(format!(
        "aterm-fleet-seed-{tag}-{}-{}",
        std::process::id(),
        aterm_uds::rand::hex_token::<4>().expect("a token")
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .expect("a private dir");
    root
}

#[test]
fn a_minted_seed_verifies_under_its_minters_key_and_fleet_alone() {
    let root = identity(1);
    let seed = FleetSeed::mint(&root, mark(1), NOW).expect("mints");
    assert!(seed.verifies(&root.key(), mark(1)));
    assert!(
        !seed.verifies(&identity(2).key(), mark(1)),
        "another node's key"
    );
    assert!(
        !seed.verifies(&root.key(), mark(2)),
        "the same root's seed of another fleet"
    );
    assert_eq!(seed.minter(), node_of(root.node()).expect("a node id"));
    assert_eq!(seed.minted_ms(), NOW);
    assert_ne!(seed.digest(), ZERO_DIGEST);
    assert_ne!(
        FleetSeed::mint(&root, mark(1), NOW).expect("mints").seed(),
        seed.seed(),
        "each mint is fresh"
    );
    for tampered in [
        FleetSeed {
            minted_ms: NOW - 1,
            ..seed
        },
        FleetSeed {
            minter: node_of(identity(2).node()).expect("a node id"),
            ..seed
        },
        FleetSeed {
            seed: [0; 16],
            ..seed
        },
    ] {
        assert!(!tampered.verifies(&root.key(), mark(1)), "{tampered:?}");
    }
}

#[test]
fn only_the_minter_signs_its_seed_again_for_a_new_secret() {
    let root = identity(1);
    let seed = FleetSeed::mint(&root, mark(1), NOW).expect("mints");
    assert_eq!(seed.resign(&identity(2), mark(2)), None, "not its minter");
    let again = seed.resign(&root, mark(2)).expect("its minter");
    assert!(again.verifies(&root.key(), mark(2)));
    assert!(!again.verifies(&root.key(), mark(1)));
    assert_eq!(
        (again.seed(), again.minted_ms(), again.digest()),
        (seed.seed(), seed.minted_ms(), seed.digest()),
        "the same seed"
    );
    assert_eq!(judge(Some(&seed), &again), Judged::Resigned);
    assert_eq!(judge(Some(&again), &again), Judged::Keep);
}

#[test]
fn the_seeds_words_spell_it_back_and_nothing_else_does() {
    let seed = FleetSeed::mint(&identity(1), mark(1), NOW).expect("mints");
    let words = seed.words();
    assert!(words.len() < LINE_MAX, "{}", words.len());
    assert_eq!(FleetSeed::of_line(&words), Some(seed));
    assert_eq!(
        FleetSeed::of_line(&format!("peer m8.local 7485 {words} name=m8")),
        Some(seed),
        "among a reply's other words"
    );
    let seed_hex = aterm_codec::hex::encode_lower(seed.seed());
    for bad in [
        words.replacen("seed_ms=", "seed_ms=x", 1),
        words.replacen(&seed_hex, &seed_hex.to_uppercase(), 1),
        words.replacen(&seed_hex, &seed_hex[2..], 1),
        format!("{words} seed_ms=1"),
        words.replacen("seed_by=", "seed_by=node-", 1),
        words.replacen("seed_ms=", "seed_ms=99999999999999999999", 1),
        words
            .split(' ')
            .filter(|word| !word.starts_with("seed_sig="))
            .collect::<Vec<_>>()
            .join(" "),
    ] {
        assert_eq!(FleetSeed::of_line(&bad), None, "{bad}");
    }
}

#[test]
fn the_debug_form_shows_no_seed() {
    let seed = FleetSeed::mint(&identity(1), mark(1), NOW).expect("mints");
    let shown = format!("{seed:?}");
    assert!(
        !shown.contains(&aterm_codec::hex::encode_lower(seed.seed())),
        "{shown}"
    );
    assert!(shown.contains(&aterm_codec::hex::encode_lower(&seed.digest())));
}

#[test]
fn a_seed_is_replaced_only_by_its_own_minters_earlier_one() {
    let root = identity(1);
    let first = FleetSeed::mint(&root, mark(1), NOW).expect("mints");
    let second = FleetSeed::mint(&root, mark(1), NOW + 1_000).expect("mints");
    let other = FleetSeed::mint(&identity(2), mark(1), NOW - 1_000).expect("mints");
    assert_eq!(judge(None, &second), Judged::Take, "the first is on trust");
    assert_eq!(judge(Some(&first), &first), Judged::Keep);
    assert_eq!(judge(Some(&first), &second), Judged::Keep, "a later mint");
    assert_eq!(judge(Some(&second), &first), Judged::Earlier);
    assert_eq!(
        judge(Some(&first), &other),
        Judged::Conflict,
        "an earlier mint of another minter re-keys nothing"
    );
    let twin = FleetSeed::mint(&root, mark(1), NOW).expect("mints");
    let (low, high) = if twin.digest() < first.digest() {
        (twin, first)
    } else {
        (first, twin)
    };
    assert_eq!(
        (judge(Some(&high), &low), judge(Some(&low), &high)),
        (Judged::Earlier, Judged::Keep),
        "at one mint time every member settles on the lower digest"
    );
}

#[test]
fn seed_and_saved_digest_files_round_trip_and_reject_a_malformed_digest() {
    let dir = private_state("seed-files");
    assert_eq!(read(&dir).expect("reads"), None);
    assert_eq!(read_digest(&dir).expect("reads"), None);
    let seed = FleetSeed::mint(&identity(1), mark(1), NOW).expect("mints");
    aterm_uds::private_file::write_private(&dir, FILE, (seed.words() + "\n").as_bytes())
        .expect("writes");
    let kept = [7; 8];
    aterm_uds::private_file::write_private(&dir, DIGEST_FILE, b"0707070707070707\n")
        .expect("writes");
    assert_eq!(read(&dir).expect("reads"), Some(seed));
    assert_eq!(read_digest(&dir).expect("reads"), Some(kept));
    aterm_uds::private_file::write_private(&dir, DIGEST_FILE, b"07070707\n").expect("writes");
    assert!(read_digest(&dir).is_err(), "a short digest");
    std::fs::remove_dir_all(&dir).expect("cleans up");
}

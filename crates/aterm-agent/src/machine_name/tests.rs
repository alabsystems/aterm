// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::unix::fs::PermissionsExt as _;

use aterm_net::fleet::NodeIdentity;
use aterm_net::fleet_member::name_record::{
    By, ByKind, FLOOR_MS, NameRecord, ZERO_DIGEST, node_of,
};
use aterm_net::fleet_member::{ISSUER_BOOK_FILE, NAME_FILE, PEERS_FILE, Peer, node_identity};
use aterm_types::machine_name::{Record, nouns};
use aterm_uds::private_file::write_private;

use super::*;

/// Late 2026, well past [`FLOOR_MS`].
const NOW: u64 = FLOOR_MS + 300 * 86_400_000;

fn state() -> aterm_tempfile::TempDir {
    let root = aterm_tempfile::tempdir().expect("a tempdir");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("a private dir");
    root
}

fn num(n: u32) -> Number {
    Number::new(n).expect("a number")
}

fn never(_: &str) -> bool {
    false
}

/// `me`'s record naming `n`, at `rev`, nicknamed `nick` when given.
fn record(me: &NodeIdentity, n: Option<u32>, rev: u64, nick: Option<&str>) -> NameRecord {
    NameRecord {
        claim: Record {
            num: n.map(num),
            class: Class::Chosen,
            since: NOW - 1_000,
            base: None,
            tgen: 0,
            sgen: 0,
            act: NOW - 1_000,
            anchor: None,
            prev: None,
            waits: false,
            ephemeral: false,
            was: Vec::new(),
            top: 9,
            cap: None,
        },
        rev,
        eseq: 5,
        boot: None,
        nick: nick.map(|word| Nickname::parse(word).expect("a nickname")),
        nick_class: NickClass::Chosen,
        by: By {
            node: node_of(me.node()).expect("a node id"),
            kind: ByKind::Person,
        },
    }
}

/// `who`'s own signed entry carrying `record` under the zero digest.
fn entry(who: &NodeIdentity, record: &NameRecord) -> String {
    let mut peer = Peer::own(who, ("m9.local", 7485, "0.160.0"), NOW);
    peer.signed = peer
        .signed
        .map(|signed| signed.with_record(record.sign(who, ZERO_DIGEST)));
    peer.line() + "\n"
}

#[test]
fn a_new_machine_is_m0_and_a_drawn_noun_and_every_later_reader_keeps_it() {
    let root = state();
    let taken = |word: &str| word.eq_ignore_ascii_case("otter");
    let (name, origin) = ensure(root.path(), None, taken, NOW).expect("ensures");
    assert_eq!(origin, Origin::Drawn);
    assert_eq!(name.num, None);
    assert_eq!(name.rev, 0, "below every record this node can sign");
    assert_ne!(name.nick.as_str(), "otter", "a noun in view is not drawn");
    assert!(nouns::NOUNS.contains(&name.nick.as_str()));
    assert!(name.title().starts_with("m0 "), "{}", name.title());
    assert_eq!(
        ensure(root.path(), Some("m4"), never, NOW).expect("ensures"),
        (name, Origin::Kept),
        "the file stands; later evidence is reconcile's"
    );
}

#[test]
fn an_upgraded_machine_keeps_its_older_name_pending_migration() {
    let root = state();
    write_private(root.path(), NAME_FILE, b"m7\n").expect("writes");
    let (name, origin) = ensure(root.path(), Some("m4"), never, NOW).expect("ensures");
    assert_eq!(origin, Origin::Evidence);
    assert_eq!(
        (name.num, name.class, name.since, name.migration),
        (
            Some(num(7)),
            Class::Seeded,
            IMPLIED_SINCE,
            Some(Migration::Pending)
        ),
        "the fleet name before the roster label"
    );
    assert_eq!(name.nick_class, NickClass::Auto, "no word to keep: drawn");

    let root = state();
    write_private(root.path(), NAME_FILE, b"studio\n").expect("writes");
    let (name, origin) = ensure(root.path(), Some("m12"), never, NOW).expect("ensures");
    assert_eq!(origin, Origin::Evidence);
    assert_eq!(
        name.title(),
        "m12 Studio",
        "the roster's NAME, the fleet name's word"
    );
    assert_eq!(name.nick_class, NickClass::Chosen);
    assert_eq!(name.migration, Some(Migration::Pending));
}

#[test]
fn only_a_bare_name_is_older_evidence() {
    for roster in ["m8-macbook-pro-m5-128gb-8tb", "8", "M8", "m08"] {
        let root = state();
        let (name, origin) = ensure(root.path(), Some(roster), never, NOW).expect("ensures");
        assert_eq!((name.num, origin), (None, Origin::Drawn), "{roster}");
    }
    let root = state();
    let (name, origin) = ensure(root.path(), Some("m8"), never, NOW).expect("ensures");
    assert_eq!(
        (name.num, origin),
        (Some(num(8)), Origin::Evidence),
        "control"
    );
}

#[test]
fn floor_brings_back_this_nodes_own_record_over_older_evidence() {
    let root = state();
    let me = node_identity(root.path()).expect("a node key");
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("an identity");
    write_private(root.path(), NAME_FILE, b"m7\n").expect("writes");
    let mine = record(&me, Some(9), 3, Some("Heron"));
    write_private(
        root.path(),
        PEERS_FILE,
        (entry(&other, &record(&other, Some(11), 8, Some("Puffin"))) + &entry(&me, &mine))
            .as_bytes(),
    )
    .expect("writes the book");
    let (name, origin) = ensure(root.path(), None, never, NOW).expect("ensures");
    assert_eq!(origin, Origin::Floor);
    assert_eq!(
        name.title(),
        "m9 Heron",
        "this node's record, not the other's"
    );
    assert_eq!(
        (name.rev, name.class, name.migration),
        (3, Class::Chosen, None)
    );
    assert_eq!(name.by, Some(mine.by));
    assert!(name.fleets.is_empty(), "the zero digest names no fleet");
}

#[test]
fn floor_without_this_nodes_record_leaves_the_older_evidence() {
    let root = state();
    node_identity(root.path()).expect("a node key");
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("an identity");
    write_private(root.path(), NAME_FILE, b"m7\n").expect("writes");
    write_private(
        root.path(),
        ISSUER_BOOK_FILE,
        entry(&other, &record(&other, Some(11), 8, None)).as_bytes(),
    )
    .expect("writes the book");
    let (name, origin) = ensure(root.path(), None, never, NOW).expect("ensures");
    assert_eq!((name.num, origin), (Some(num(7)), Origin::Evidence));
}

#[test]
fn a_waiting_record_keeps_its_name_in_the_title() {
    let root = state();
    let me = node_identity(root.path()).expect("a node key");
    let mut waiting = record(&me, None, 4, None);
    waiting.claim.waits = true;
    waiting.claim.prev = Some(num(8));
    write_private(
        root.path(),
        ISSUER_BOOK_FILE,
        entry(&me, &waiting).as_bytes(),
    )
    .expect("writes the book");
    let (name, origin) = ensure(root.path(), None, never, NOW).expect("ensures");
    assert_eq!(origin, Origin::Floor);
    assert_eq!(
        (name.num, name.prev, name.migration),
        (Some(num(8)), None, Some(Migration::Waiting))
    );
    assert!(
        name.title().starts_with("m8 "),
        "never m0: {}",
        name.title()
    );
    assert_eq!(
        name.nick_class,
        NickClass::Auto,
        "no nickname in the record: drawn"
    );
}

#[test]
fn a_displaced_record_keeps_its_last_name_in_the_title() {
    let root = state();
    let me = node_identity(root.path()).expect("a node key");
    let mut moving = record(&me, None, 6, Some("Heron"));
    moving.claim.prev = Some(num(8));
    write_private(root.path(), PEERS_FILE, entry(&me, &moving).as_bytes())
        .expect("writes the book");
    let (name, origin) = ensure(root.path(), None, never, NOW).expect("ensures");
    assert_eq!(origin, Origin::Floor);
    assert_eq!(name.title(), "m8 Heron", "never m0 on a numbered machine");
    assert_eq!((name.prev, name.migration), (None, None));

    let root = state();
    let me = node_identity(root.path()).expect("a node key");
    let never_numbered = record(&me, None, 2, Some("Otter"));
    write_private(
        root.path(),
        PEERS_FILE,
        entry(&me, &never_numbered).as_bytes(),
    )
    .expect("writes the book");
    let (name, _) = ensure(root.path(), None, never, NOW).expect("ensures");
    assert_eq!(name.title(), "m0 Otter", "control: no NAME kept");
}

#[test]
fn floor_under_the_held_seed_keeps_that_fleets_claim() {
    use aterm_net::fleet_member::fleet_seed::{self, FleetSeed};
    let root = state();
    let me = node_identity(root.path()).expect("a node key");
    let mark = aterm_net::fleet::FleetMark::from_hex("0101010101010101").expect("a mark");
    let seed = FleetSeed::mint(&me, mark, NOW).expect("mints");
    write_private(
        root.path(),
        fleet_seed::FILE,
        format!("{}\n", seed.words()).as_bytes(),
    )
    .expect("writes the seed");
    let mine = record(&me, Some(9), 3, Some("Heron"));
    let mut peer = Peer::own(&me, ("m9.local", 7485, "0.160.0"), NOW);
    peer.signed = peer
        .signed
        .map(|signed| signed.with_record(mine.sign(&me, seed.digest())));
    write_private(root.path(), PEERS_FILE, (peer.line() + "\n").as_bytes())
        .expect("writes the book");
    let (name, origin) = ensure(root.path(), None, never, NOW).expect("ensures");
    assert_eq!(origin, Origin::Floor);
    assert_eq!(name.title(), "m9 Heron");
    assert_eq!(
        name.fleets
            .iter()
            .map(|fleet| (fleet.digest, fleet.top, fleet.claim.num))
            .collect::<Vec<_>>(),
        vec![(seed.digest(), 9, Some(num(9)))],
        "the claim kept under the seed it was signed under"
    );
}

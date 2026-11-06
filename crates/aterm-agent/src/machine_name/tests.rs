// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;

use aterm_net::fleet::NodeIdentity;
use aterm_net::fleet_member::name_record::{
    By, ByKind, FLOOR_MS, NameRecord, ZERO_DIGEST, node_of,
};
use aterm_net::fleet_member::{ISSUER_BOOK_FILE, NAME_FILE, PEERS_FILE, Peer, node_identity};
use aterm_secrets::client::{LabLabels, LabelHold};
use aterm_types::machine_name::{Record, nouns};
use aterm_uds::private_file::write_private;

use super::*;

fn no_labels() -> io::Result<LabLabels> {
    Ok(LabLabels::default())
}

/// Late 2026, well past [`FLOOR_MS`].
pub(crate) const NOW: u64 = FLOOR_MS + 300 * 86_400_000;

pub(crate) fn state() -> aterm_tempfile::TempDir {
    let root = aterm_tempfile::tempdir().expect("a tempdir");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("a private dir");
    root
}

#[test]
fn first_name_evidence_is_read_without_holding_the_name_lock() {
    use aterm_uds::private_file::PrivateLock;
    use std::time::Duration;

    let root = state();
    let roster = || {
        let _lock = PrivateLock::take(
            root.path(),
            file::LOCK,
            Duration::ZERO,
            "the name lock was already held",
        )?;
        Ok(Some("m86".to_owned()))
    };
    let (name, origin) =
        ensure(root.path(), roster, never, NOW).expect("the roster can acquire the name lock");
    assert_eq!((name.num, origin), (Some(num(86)), Origin::Evidence));
}

#[test]
fn a_concurrent_first_reader_wins_even_when_the_other_read_fails() {
    use aterm_uds::private_file::PrivateLock;
    use std::time::Duration;

    for fails in [false, true] {
        let root = state();
        let winner = MachineName::unnumbered(Nickname::parse("Winner").expect("a nickname"), NOW);
        let roster = || {
            let _lock = PrivateLock::take(
                root.path(),
                file::LOCK,
                Duration::ZERO,
                "the candidate read held the name lock",
            )?;
            write_private(
                root.path(),
                file::FILE,
                winner.to_toml().expect("the winning file").as_bytes(),
            )?;
            if fails {
                Err(io::Error::other(
                    "this reader's roster failed after the winner arrived",
                ))
            } else {
                Ok(Some("m86".to_owned()))
            }
        };
        assert_eq!(
            ensure(root.path(), roster, never, NOW).expect("the valid winner stands"),
            (winner.clone(), Origin::Kept),
            "fails={fails}"
        );
        assert_eq!(
            file::read(root.path()).expect("reads the winner"),
            Some(winner)
        );
    }
}

fn num(n: u32) -> Number {
    Number::new(n).expect("a number")
}

fn never(_: &str) -> bool {
    false
}

fn no_roster() -> io::Result<Option<String>> {
    Ok(None)
}

/// A roster that gives `word`.
fn label(word: &str) -> impl FnOnce() -> io::Result<Option<String>> + '_ {
    move || Ok(Some(word.to_owned()))
}

/// A roster a test must never read.
fn unread() -> io::Result<Option<String>> {
    unreachable!("the roster is read only when nothing else names the claim")
}

/// `me`'s record naming `n`, at `rev`, nicknamed `nick` when given.
pub(crate) fn record(
    me: &NodeIdentity,
    n: Option<u32>,
    rev: u64,
    nick: Option<&str>,
) -> NameRecord {
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
pub(crate) fn entry(who: &NodeIdentity, record: &NameRecord) -> String {
    entry_under(who, record, ZERO_DIGEST)
}

/// `who`'s own signed entry carrying `record` signed under seed digest `digest`.
pub(crate) fn entry_under(who: &NodeIdentity, record: &NameRecord, digest: [u8; 8]) -> String {
    let mut peer = Peer::own(who, ("m9.local", 7485, "0.160.0"), NOW);
    peer.signed = peer
        .signed
        .map(|signed| signed.with_record(record.sign(who, digest)));
    peer.line() + "\n"
}

#[test]
fn a_new_machine_is_m0_and_a_drawn_noun_and_every_later_reader_keeps_it() {
    let root = state();
    let taken = |word: &str| word.eq_ignore_ascii_case("otter");
    let (name, origin) = ensure(root.path(), no_roster, taken, NOW).expect("ensures");
    assert_eq!(origin, Origin::Drawn);
    assert_eq!(name.num, None);
    assert_eq!(name.rev, 0, "below every record this node can sign");
    assert_ne!(name.nick.as_str(), "otter", "a noun in view is not drawn");
    assert!(nouns::NOUNS.contains(&name.nick.as_str()));
    assert!(name.title().starts_with("m0 "), "{}", name.title());
    assert_eq!(
        ensure(root.path(), unread, never, NOW).expect("ensures"),
        (name, Origin::Kept),
        "the file stands; later evidence is reconcile's"
    );
}

#[test]
fn a_chosen_number_and_nickname_are_one_change_with_the_prior_claim_kept() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let to = NameTo {
        num: num(86),
        nick: Some(Nickname::parse("MissBunny").expect("a nickname")),
    };
    let set = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("renames");
    assert!(set.changed);
    assert_eq!(set.name.title(), "m86 MissBunny");
    assert_eq!(set.name.class, Class::Chosen);
    assert_eq!(set.name.nick_class, NickClass::Chosen);
    assert_eq!(set.name.rev, before.rev + 1);
    assert!(set.name.since > before.act);
    assert_eq!(set.name.since, set.name.act);
    assert_eq!(set.name.prev, before.num);
    assert_eq!(set.name.was[0].num, num(7));
    assert_eq!(set.name.migration, None);
    let undo = set.name.undo.as_ref().expect("the old claim");
    assert_eq!(
        (undo.num, undo.class, undo.since),
        (before.num, before.class, before.since)
    );
    assert_eq!(
        file::read(root.path()).expect("reads"),
        Some(set.name.clone())
    );

    let again = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Agent,
        unread,
        NOW + 1,
        false,
    )
    .expect("an unchanged request");
    assert!(!again.changed);
    assert_eq!(
        again.name, set.name,
        "no new revision, actor or undo for a no-op"
    );
}

#[test]
fn renumbering_without_a_nickname_keeps_the_word_and_preview_changes_no_name_or_key() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let to = NameTo {
        num: num(86),
        nick: None,
    };
    let preview = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Agent,
        unread,
        NOW,
        true,
    )
    .expect("previews");
    assert!(preview.changed);
    assert_eq!(
        (preview.name.num, &preview.name.nick),
        (Some(num(86)), &before.nick)
    );
    assert_eq!(
        file::read(root.path()).expect("reads"),
        Some(before.clone())
    );
    assert!(
        read_node_identity(root.path())
            .expect("reads a key")
            .is_none()
    );
    let set = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Agent,
        unread,
        NOW,
        false,
    )
    .expect("renames");
    assert_eq!((set.name.num, set.name.nick), (Some(num(86)), before.nick));
    assert_eq!(set.name.by.expect("an actor").kind, ByKind::Agent);
}

#[test]
fn another_signed_number_is_refused_without_a_partial_nickname_change() {
    let root = state();
    let me = node_identity(root.path()).expect("our identity");
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let to = NameTo {
        num: num(86),
        nick: Some(Nickname::parse("MissBunny").expect("a nickname")),
    };
    for class in [Class::Chosen, Class::Seeded, Class::Auto] {
        let mut theirs = record(&other, Some(86), 3, Some("Otter"));
        theirs.claim.class = class;
        write_private(root.path(), PEERS_FILE, entry(&other, &theirs).as_bytes())
            .expect("writes their signed claim");
        assert!(
            set_name(
                root.path(),
                &to,
                no_labels,
                ByKind::Person,
                unread,
                NOW,
                false
            )
            .is_err()
        );
        assert_eq!(
            file::read(root.path()).expect("reads"),
            Some(before.clone())
        );
    }
    let peer = Peer::own(&other, ("m86.local", 7485, "0.160.0"), NOW)
        .with_primary_name(&other, Some("m86"));
    write_private(root.path(), PEERS_FILE, (peer.line() + "\n").as_bytes())
        .expect("writes an older signed primary");
    assert!(
        set_name(
            root.path(),
            &to,
            no_labels,
            ByKind::Person,
            unread,
            NOW,
            false
        )
        .is_err()
    );
    assert_eq!(file::read(root.path()).expect("reads"), Some(before));
    assert_eq!(
        read_node_identity(root.path())
            .expect("reads")
            .expect("our identity")
            .node(),
        me.node()
    );
}

#[test]
fn an_unread_book_never_counts_as_a_free_number() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    write_private(root.path(), PEERS_FILE, b"not a peer\n").expect("a malformed book");
    let to = NameTo {
        num: num(86),
        nick: None,
    };
    assert!(
        set_name(
            root.path(),
            &to,
            no_labels,
            ByKind::Person,
            unread,
            NOW,
            false
        )
        .is_err()
    );
    assert_eq!(file::read(root.path()).expect("reads"), Some(before));
}

#[test]
fn an_unread_fleet_key_cannot_turn_its_occupied_number_into_a_free_one() {
    use aterm_net::fleet::{KEY_FILE, read_key_file};
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    write_private(root.path(), KEY_FILE, ("ab".repeat(32) + "\n").as_bytes()).expect("a fleet key");
    let mark = read_key_file(root.path())
        .expect("reads the fleet key")
        .mark();
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
    let theirs = record(&other, Some(86), 3, Some("Otter"));
    let mut peer = Peer::own(&other, ("m86.local", 7485, "0.160.0"), NOW);
    peer.fleet = Some(mark);
    peer.signed = peer
        .signed
        .map(|signed| signed.with_record(theirs.sign(&other, ZERO_DIGEST)));
    write_private(root.path(), PEERS_FILE, (peer.line() + "\n").as_bytes()).expect("a signed peer");
    let to = NameTo {
        num: num(86),
        nick: None,
    };
    assert!(
        set_name(
            root.path(),
            &to,
            no_labels,
            ByKind::Person,
            unread,
            NOW,
            false
        )
        .is_err()
    );
    write_private(root.path(), KEY_FILE, b"not a fleet secret\n").expect("a damaged key");
    let why = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect_err("a damaged key must stop the rename");
    assert!(why.to_string().contains(KEY_FILE), "{why}");
    assert_eq!(file::read(root.path()).expect("reads"), Some(before));
}

#[test]
fn a_newer_wrapper_cannot_roll_back_or_strip_a_verified_number() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
    let mut latest = record(&other, Some(86), 3, Some("Otter"));
    latest.eseq = 20;
    let mut stale = record(&other, Some(40), 2, Some("Otter"));
    stale.eseq = 10;
    write_private(root.path(), PEERS_FILE, entry(&other, &latest).as_bytes())
        .expect("the latest record");
    let newer_wrapper = Peer::own(&other, ("m40.local", 7485, "0.160.0"), NOW + 1);
    let mut replay = newer_wrapper.clone();
    replay.signed = replay
        .signed
        .map(|signed| signed.with_record(stale.sign(&other, ZERO_DIGEST)));
    let to = NameTo {
        num: num(86),
        nick: None,
    };
    for peer in [replay, newer_wrapper] {
        write_private(
            root.path(),
            ISSUER_BOOK_FILE,
            (peer.line() + "\n").as_bytes(),
        )
        .expect("a newer wrapper with a stale or stripped record");
        assert!(
            set_name(
                root.path(),
                &to,
                no_labels,
                ByKind::Person,
                unread,
                NOW,
                false
            )
            .is_err()
        );
        assert_eq!(
            file::read(root.path()).expect("reads"),
            Some(before.clone())
        );
    }
}

#[test]
fn choosing_the_held_seeded_number_never_weakens_its_claim_against_a_rival() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
    write_private(
        root.path(),
        PEERS_FILE,
        entry(&other, &record(&other, Some(7), 3, None)).as_bytes(),
    )
    .expect("a rival's later chosen claim");
    let to = NameTo {
        num: num(7),
        nick: None,
    };
    let same = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("the current seeded holder");
    assert!(!same.changed);
    assert_eq!(same.name, before);
    let to = NameTo {
        num: num(7),
        nick: Some(Nickname::parse("MissBunny").expect("a nickname")),
    };
    let nick = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("only the nickname changes");
    assert!(nick.changed);
    assert_eq!(nick.name.title(), "m7 MissBunny");
    assert_eq!(
        (nick.name.class, nick.name.since, nick.name.act),
        (before.class, before.since, before.act)
    );
    assert_eq!(
        (nick.name.undo, nick.name.prev, nick.name.was),
        (before.undo, before.prev, before.was)
    );
}

#[test]
fn an_upgraded_machine_keeps_its_older_name_pending_migration() {
    let root = state();
    write_private(root.path(), NAME_FILE, b"m7\n").expect("writes");
    let (name, origin) = ensure(root.path(), unread, never, NOW).expect("ensures");
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
    let (name, origin) = ensure(root.path(), label("m12"), never, NOW).expect("ensures");
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
fn the_roster_is_read_only_when_nothing_else_names_the_claim() {
    let root = state();
    let me = node_identity(root.path()).expect("a node key");
    write_private(
        root.path(),
        PEERS_FILE,
        entry(&me, &record(&me, Some(9), 3, None)).as_bytes(),
    )
    .expect("writes the book");
    let (name, origin) = ensure(root.path(), unread, never, NOW).expect("ensures");
    assert_eq!((name.num, origin), (Some(num(9)), Origin::Floor));
}

#[test]
fn a_source_that_cannot_be_read_makes_nothing_and_the_next_reader_tries_again() {
    use aterm_net::fleet_member::NODE_KEY_FILE;
    let root = state();
    let busy = || -> io::Result<Option<String>> { Err(io::Error::other("the roster is busy")) };
    let refused = ensure(root.path(), busy, never, NOW).expect_err("an unread roster");
    assert!(refused.to_string().contains("busy"), "{refused}");
    assert_eq!(
        file::read(root.path()).expect("reads"),
        None,
        "nothing kept"
    );
    let (name, origin) = ensure(root.path(), label("m8"), never, NOW).expect("ensures");
    assert_eq!(
        (name.num, origin),
        (Some(num(8)), Origin::Evidence),
        "the next reader's roster"
    );

    for unreadable in [NAME_FILE, NODE_KEY_FILE] {
        let root = state();
        node_identity(root.path()).expect("a node key");
        write_private(root.path(), NAME_FILE, b"m7\n").expect("writes");
        let path = root.path().join(unreadable);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("opens it to others");
        let refused = ensure(root.path(), no_roster, never, NOW).expect_err(unreadable);
        assert!(refused.to_string().contains(unreadable), "{refused}");
        assert_eq!(
            file::read(root.path()).expect("reads"),
            None,
            "{unreadable}: nothing kept"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("private again");
        let (name, _) = ensure(root.path(), no_roster, never, NOW).expect(unreadable);
        assert_eq!(name.num, Some(num(7)), "{unreadable}: the next reader's");
    }

    let root = state();
    write_private(root.path(), NAME_FILE, b"not a name!\n").expect("writes");
    let (name, origin) = ensure(root.path(), no_roster, never, NOW).expect("ensures");
    assert_eq!(
        (name.num, origin),
        (None, Origin::Drawn),
        "text that is no name is no evidence, and no failure"
    );
}

#[test]
fn a_kept_name_reads_whatever_the_node_keys_state() {
    use aterm_net::fleet_member::NODE_KEY_FILE;
    let root = state();
    node_identity(root.path()).expect("a node key");
    let (made, _) = ensure_in_view(root.path(), no_roster, NOW).expect("makes");
    let path = root.path().join(NODE_KEY_FILE);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("opens it to others");
    let (kept, origin) = ensure_in_view(root.path(), unread, NOW).expect("reads the kept name");
    assert_eq!((kept, origin), (made, Origin::Kept));
    std::fs::remove_file(root.path().join(file::FILE)).expect("removes");
    let refused = ensure_in_view(root.path(), no_roster, NOW).expect_err("makes none");
    assert!(refused.to_string().contains(NODE_KEY_FILE), "{refused}");
}

#[test]
fn a_private_state_root_reads_no_vault_roster() {
    assert!(aterm_types::dirs::runs_under_private_state_root());
    assert_eq!(vault_roster_label().expect("reads no vault"), None);
}

#[test]
fn only_a_bare_name_is_older_evidence() {
    for roster in ["m8-macbook-pro-m5-128gb-8tb", "8", "M8", "m08"] {
        let root = state();
        let (name, origin) = ensure(root.path(), label(roster), never, NOW).expect("ensures");
        assert_eq!((name.num, origin), (None, Origin::Drawn), "{roster}");
    }
    let root = state();
    let (name, origin) = ensure(root.path(), label("m8"), never, NOW).expect("ensures");
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
    let (name, origin) = ensure(root.path(), no_roster, never, NOW).expect("ensures");
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
    let (name, origin) = ensure(root.path(), no_roster, never, NOW).expect("ensures");
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
    let (name, origin) = ensure(root.path(), no_roster, never, NOW).expect("ensures");
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
    let (name, origin) = ensure(root.path(), no_roster, never, NOW).expect("ensures");
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
    let (name, _) = ensure(root.path(), no_roster, never, NOW).expect("ensures");
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
    let (name, origin) = ensure(root.path(), no_roster, never, NOW).expect("ensures");
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

#[test]
fn the_nicknames_taken_are_other_nodes_records_under_the_held_seed() {
    let root = state();
    let me = node_identity(root.path()).expect("a node");
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("an identity");
    let mark = aterm_net::fleet::FleetMark::from_hex("0101010101010101").expect("a mark");
    let seed = aterm_net::fleet_member::fleet_seed::FleetSeed::mint(&me, mark, NOW).expect("mints");
    write_private(
        root.path(),
        aterm_net::fleet_member::fleet_seed::FILE,
        format!("{}\n", seed.words()).as_bytes(),
    )
    .expect("writes the seed");
    write_private(
        root.path(),
        ISSUER_BOOK_FILE,
        (entry(&me, &record(&me, Some(9), 3, Some("Heron")))
            + &entry_under(
                &other,
                &record(&other, Some(11), 4, Some("Puffin")),
                seed.digest(),
            ))
            .as_bytes(),
    )
    .expect("writes the book");
    assert_eq!(
        taken_in(root.path(), Some(me.node())),
        [Nickname::parse("Puffin").expect("a nickname")],
        "the other node's, under the seed held here; never this node's own"
    );
}

#[test]
fn a_nickname_in_view_is_one_in_any_case() {
    let taken = [Nickname::parse("Heron").expect("a nickname")];
    assert!(in_view(&taken, "heron") && in_view(&taken, "HERON"));
    assert!(!in_view(&taken, "otter"), "control");
}

#[test]
fn a_nickname_change_is_chosen_raises_rev_and_keeps_the_claim() {
    let heron = || Nickname::parse("heron").expect("a nickname");
    let held = MachineName::unnumbered(heron(), NOW);
    let chosen = renicked(&held, heron(), None).expect("a drawn noun chosen is a change");
    assert_eq!(
        (chosen.nick_class, chosen.rev),
        (NickClass::Chosen, 0),
        "a file no record seeded stays at rev 0, so Floor still adopts this node's own claim"
    );
    assert_eq!(renicked(&chosen, heron(), None), None, "chosen already");
    let seeded = MachineName { rev: 4, ..chosen };
    let respelled = renicked(&seeded, Nickname::parse("Heron").expect("a nickname"), None)
        .expect("another spelling is another nickname");
    assert_eq!(respelled.rev, 5, "a seeded file's change raises rev");
    assert_eq!(
        (
            respelled.num,
            respelled.class,
            respelled.since,
            respelled.act
        ),
        (held.num, held.class, held.since, held.act),
        "the claim as it was"
    );
}

/// IN A FLEET IS THE VAULT'S STANDING (§11.85): a member is in; let in, asking or seatless is
/// joining; never in, taken out or let go is out; an unreadable state says why; and a fleet key
/// that reads stands for membership, until the one-mesh design's D2, wherever the vault did not
/// take this Mac out or let it go (`members_cli`'s
/// `a_vault_that_took_this_mac_out_outranks_its_fleet_key` reads a real one).
#[test]
fn in_a_fleet_is_the_vaults_standing_or_a_fleet_key() {
    use aterm_secrets::members::join::JoinPlace;
    let cases = [
        (Standing::Member("m8".to_owned()), InFleet::In),
        (Standing::Seatless, InFleet::Joining),
        (
            Standing::Allowed {
                fix: None,
                held: false,
            },
            InFleet::Joining,
        ),
        (Standing::Asking(JoinPlace::Unknown), InFleet::Joining),
        (Standing::NotIn, InFleet::Out),
        (Standing::Dropped("let go".to_owned()), InFleet::Out),
        (
            Standing::Unknown("state.toml: bad".to_owned()),
            InFleet::Unread("state.toml: bad".to_owned()),
        ),
    ];
    for (standing, want) in &cases {
        assert_eq!(&InFleet::of(false, false, standing), want, "{standing:?}");
        let keyed = if matches!(standing, Standing::Dropped(_)) {
            InFleet::Out
        } else {
            InFleet::In
        };
        assert_eq!(
            InFleet::of(true, false, standing),
            keyed,
            "a key: {standing:?}"
        );
        assert_eq!(
            InFleet::of(true, true, standing),
            InFleet::Out,
            "taken out, with a key: {standing:?}"
        );
    }
    let words: Vec<_> = [
        InFleet::In,
        InFleet::Joining,
        InFleet::Out,
        InFleet::Unread(String::new()),
    ]
    .iter()
    .map(|f| (f.word(), f.phrase()))
    .collect();
    assert_eq!(
        words,
        [
            ("in", "in a fleet"),
            ("joining", "joining a fleet"),
            ("out", "not in a fleet"),
            ("unread", "fleet not read"),
        ]
    );
}

/// The read: no fleet key and a home with no vault is out, and a fleet key that reads is in.
/// Under a private state root (a test binary) no vault is read: `$HOME`'s is the person's.
#[test]
fn a_fleet_key_is_in_and_a_home_with_no_vault_is_out() {
    let root = state();
    let vault = aterm_secrets::client::Paths::locate_for_home(&root.path().join("home"));
    assert_eq!(InFleet::read(root.path(), Some(&vault)), InFleet::Out);
    let key = format!("{}\n", "ab".repeat(32));
    write_private(root.path(), aterm_net::fleet::KEY_FILE, key.as_bytes()).expect("a fleet key");
    assert_eq!(InFleet::read(root.path(), Some(&vault)), InFleet::In);
    assert!(super::standing_vault().is_none(), "a test reads no vault");
}

/// A restored local backup behind this node's own signed claim, with an obsolete undo entry.
fn edit_floor_fixture() -> (
    aterm_tempfile::TempDir,
    NodeIdentity,
    MachineName,
    NameRecord,
) {
    let root = state();
    let me = node_identity(root.path()).expect("our identity");
    ensure(root.path(), label("m7"), never, NOW).expect("the old name");
    let before = file::replace(root.path(), |name| {
        name.rev = 2;
        name.nick = Nickname::parse("Heron").expect("a nickname");
        name.undo = Some(file::Held {
            num: Some(num(6)),
            class: Class::Chosen,
            since: NOW - 2_000,
            base: None,
            tgen: 0,
            sgen: 0,
        });
        Ok(())
    })
    .expect("the local backup");
    let mut signed = record(&me, Some(9), 9, Some("Otter"));
    signed.claim.prev = Some(num(8));
    signed.claim.was.push(aterm_types::machine_name::Was {
        num: num(8),
        retired: true,
        lost: false,
    });
    signed.claim.cap = Some(aterm_types::machine_name::Cap {
        value: 100,
        since: NOW - 500,
    });
    write_private(root.path(), PEERS_FILE, entry(&me, &signed).as_bytes())
        .expect("our newer signed claim");
    (root, me, before, signed)
}

#[test]
fn choosing_a_recovered_waiting_number_creates_a_claim_only_when_available() {
    for occupied in [false, true] {
        let (root, me, before, mut signed) = edit_floor_fixture();
        signed.claim.num = None;
        signed.claim.prev = Some(num(9));
        signed.claim.waits = true;
        let mut book = entry(&me, &signed);
        if occupied {
            let other =
                NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
            book.push_str(&entry(&other, &record(&other, Some(9), 3, Some("Heron"))));
        }
        write_private(root.path(), PEERS_FILE, book.as_bytes()).expect("the waiting record");
        let result = set_name(
            root.path(),
            &NameTo {
                num: num(9),
                nick: None,
            },
            no_labels,
            ByKind::Agent,
            unread,
            NOW,
            false,
        );
        if occupied {
            assert!(result.is_err(), "another machine holds this number");
            assert_eq!(file::read(root.path()).expect("reads"), Some(before));
        } else {
            let set = result.expect("chooses the available number");
            assert_eq!(set.name.num, Some(num(9)));
            assert_eq!(set.name.migration, None, "the request leaves Waiting");
            assert_eq!(set.name.class, Class::Chosen);
            assert_eq!(set.name.rev, signed.rev + 1);
            assert_eq!(set.name.nick.as_str(), "Otter");
            assert_eq!(set.name.by.expect("the edit's actor").kind, ByKind::Agent);
            assert_eq!(
                set.name.undo.expect("the recovered claim").num,
                Some(num(9))
            );
        }
    }
}

#[test]
fn name_and_nickname_edits_restore_the_own_signed_claim_before_acting() {
    for number in [true, false] {
        let (root, _, _, signed) = edit_floor_fixture();
        let nick = Nickname::parse("MissBunny").expect("a nickname");
        let (name, origin) = if number {
            let set = set_name(
                root.path(),
                &NameTo {
                    num: num(86),
                    nick: Some(nick),
                },
                no_labels,
                ByKind::Agent,
                unread,
                NOW,
                false,
            )
            .expect("renames from the floor");
            assert!(set.changed);
            (set.name, set.origin)
        } else {
            let set = set_nick(
                root.path(),
                &NickTo::Word(nick),
                ByKind::Agent,
                unread,
                NOW,
                false,
            )
            .expect("renicks from the floor");
            assert!(set.changed);
            (set.name, set.origin)
        };
        assert_eq!(origin, Origin::Floor);
        assert_eq!(name.rev, signed.rev + 1);
        assert_eq!(name.nick.as_str(), "MissBunny");
        assert_eq!(name.cap, signed.claim.cap);
        assert_eq!(name.by.expect("the new actor").kind, ByKind::Agent);
        if number {
            assert_eq!(name.num, Some(num(86)));
            let undo = name.undo.as_ref().expect("the restored claim is undo");
            assert_eq!(
                (undo.num, undo.class, undo.since),
                (signed.claim.num, signed.claim.class, signed.claim.since)
            );
            assert_eq!(
                name.was.iter().map(|was| was.num).collect::<Vec<_>>(),
                [num(9), num(8)]
            );
        } else {
            assert_eq!(
                (name.num, name.class, name.since, name.act),
                (
                    signed.claim.num,
                    signed.claim.class,
                    signed.claim.since,
                    signed.claim.act
                )
            );
            assert_eq!(name.was, signed.claim.was);
            assert!(
                name.undo.is_none(),
                "the stale local undo is not the restored claim's undo"
            );
        }
        assert_eq!(file::read(root.path()).expect("reads"), Some(name));
    }
}

#[test]
fn a_request_already_matching_the_floor_persists_its_repair_without_an_extra_revision() {
    for number in [true, false] {
        let (root, _, _, signed) = edit_floor_fixture();
        let (name, changed, origin) = if number {
            let set = set_name(
                root.path(),
                &NameTo {
                    num: num(9),
                    nick: None,
                },
                no_labels,
                ByKind::Agent,
                unread,
                NOW,
                false,
            )
            .expect("restores the held number");
            (set.name, set.changed, set.origin)
        } else {
            let set = set_nick(
                root.path(),
                &NickTo::Word(signed.nick.clone().expect("a nickname")),
                ByKind::Agent,
                unread,
                NOW,
                false,
            )
            .expect("restores the held nickname");
            (set.name, set.changed, set.origin)
        };
        assert!(changed, "restoring a backup changed the local file");
        assert_eq!(origin, Origin::Floor);
        assert_eq!(
            (name.num, name.rev, name.by),
            (signed.claim.num, signed.rev, Some(signed.by))
        );
        assert_eq!(
            (name.class, name.since, name.act),
            (signed.claim.class, signed.claim.since, signed.claim.act)
        );
        assert_eq!(name.was, signed.claim.was);
        assert!(name.undo.is_none());
        assert_eq!(file::read(root.path()).expect("reads"), Some(name));
    }
}

#[test]
fn edit_floor_previews_restore_and_edit_without_replacing_the_local_backup() {
    for number in [true, false] {
        let (root, _, before, signed) = edit_floor_fixture();
        let nick = Nickname::parse("MissBunny").expect("a nickname");
        let (name, changed, origin) = if number {
            let set = set_name(
                root.path(),
                &NameTo {
                    num: num(86),
                    nick: Some(nick),
                },
                no_labels,
                ByKind::Agent,
                unread,
                NOW,
                true,
            )
            .expect("previews a rename");
            (set.name, set.changed, set.origin)
        } else {
            let set = set_nick(
                root.path(),
                &NickTo::Word(nick),
                ByKind::Agent,
                unread,
                NOW,
                true,
            )
            .expect("previews a nickname");
            (set.name, set.changed, set.origin)
        };
        assert!(changed);
        assert_eq!(origin, Origin::Floor);
        assert_eq!(name.rev, signed.rev + 1);
        assert_eq!(name.nick.as_str(), "MissBunny");
        assert_eq!(file::read(root.path()).expect("reads"), Some(before));
    }
}

#[test]
fn floor_edits_ignore_other_keys_digests_and_records_not_above_the_local_revision() {
    for number in [true, false] {
        for control in ["other-key", "other-digest", "older", "equal"] {
            let (root, me, before, mut signed) = edit_floor_fixture();
            match control {
                "older" => signed.rev = before.rev - 1,
                "equal" => signed.rev = before.rev,
                _ => {}
            }
            let entry = match control {
                "other-key" => {
                    let impostor =
                        NodeIdentity::from_seed(me.node(), &[0x42; 32]).expect("another key");
                    entry(&impostor, &signed)
                }
                "other-digest" => entry_under(&me, &signed, [0x42; 8]),
                _ => entry(&me, &signed),
            };
            write_private(root.path(), PEERS_FILE, entry.as_bytes()).expect("the control record");
            let nick = Nickname::parse("MissBunny").expect("a nickname");
            let (name, origin) = if number {
                let set = set_name(
                    root.path(),
                    &NameTo {
                        num: num(86),
                        nick: Some(nick),
                    },
                    no_labels,
                    ByKind::Agent,
                    unread,
                    NOW,
                    false,
                )
                .expect("renames local state");
                (set.name, set.origin)
            } else {
                let set = set_nick(
                    root.path(),
                    &NickTo::Word(nick),
                    ByKind::Agent,
                    unread,
                    NOW,
                    false,
                )
                .expect("renicks local state");
                (set.name, set.origin)
            };
            assert_eq!(origin, Origin::Kept, "{control}");
            assert_eq!(name.rev, before.rev + 1, "{control}");
            if number {
                assert_eq!(name.undo.expect("local undo").num, before.num, "{control}");
            } else {
                assert_eq!(
                    (name.num, name.undo),
                    (before.num, before.undo),
                    "{control}"
                );
            }
        }
    }
}

#[test]
fn edits_use_the_floor_record_ordered_by_entry_sequence_before_revision() {
    let (root, me, _, mut latest) = edit_floor_fixture();
    latest.eseq = 20;
    let mut older = record(&me, Some(10), 50, Some("Older"));
    older.eseq = 10;
    write_private(root.path(), PEERS_FILE, entry(&me, &latest).as_bytes())
        .expect("newer publication");
    write_private(root.path(), ISSUER_BOOK_FILE, entry(&me, &older).as_bytes())
        .expect("older publication");
    let set = set_name(
        root.path(),
        &NameTo {
            num: num(86),
            nick: None,
        },
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("edits the newer publication");
    assert_eq!(set.name.rev, latest.rev + 1);
    assert_eq!(set.name.nick, latest.nick.expect("the newest nickname"));
    assert_eq!(
        set.name.undo.expect("the latest claim").num,
        latest.claim.num
    );
}

#[test]
fn a_saved_seed_digest_and_a_future_own_clock_do_not_lose_the_edit_floor() {
    let (root, me, _, mut signed) = edit_floor_fixture();
    let digest = [0x42; 8];
    signed.claim.since = NOW + 3 * 86_400_000;
    signed.claim.act = signed.claim.since;
    write_private(root.path(), fleet_seed::DIGEST_FILE, b"4242424242424242\n")
        .expect("the digest kept after a leave");
    write_private(
        root.path(),
        PEERS_FILE,
        entry_under(&me, &signed, digest).as_bytes(),
    )
    .expect("our signed record under the saved digest");
    let set = set_name(
        root.path(),
        &NameTo {
            num: num(86),
            nick: None,
        },
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("edits the signed floor despite a clock reset");
    assert_eq!(set.origin, Origin::Floor);
    assert_eq!(set.name.rev, signed.rev + 1);
    assert!(set.name.since > signed.claim.act);
    assert_eq!(
        set.name.undo.expect("the restored claim").num,
        signed.claim.num
    );
    assert_eq!(set.name.fleets[0].digest, digest);
}

#[test]
fn a_conflicting_rename_does_not_partially_persist_its_floor_repair() {
    let (root, me, before, signed) = edit_floor_fixture();
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
    let peers = entry(&me, &signed) + &entry(&other, &record(&other, Some(86), 3, None));
    write_private(root.path(), PEERS_FILE, peers.as_bytes()).expect("our floor and their claim");
    let to = NameTo {
        num: num(86),
        nick: Some(Nickname::parse("MissBunny").expect("a nickname")),
    };
    assert!(
        set_name(
            root.path(),
            &to,
            no_labels,
            ByKind::Person,
            unread,
            NOW,
            false
        )
        .is_err()
    );
    assert_eq!(file::read(root.path()).expect("reads"), Some(before));
}

/// The vault's labels on today's lab (§11.86): a number its roster, log or policy gives another
/// Mac or role is refused with what holds it, changing nothing; the same number with no labels
/// read renames, which shows the labels are what refuse.
#[test]
fn a_number_the_vault_gives_another_mac_is_refused_with_its_holder() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let to = NameTo {
        num: num(20),
        nick: Some(Nickname::parse("Heron").expect("a nickname")),
    };
    for hold in [LabelHold::Label, LabelHold::Seat, LabelHold::Reserved] {
        let lab = LabLabels {
            held: BTreeMap::from([(num(20), hold)]),
            ..LabLabels::default()
        };
        let why = set_name(
            root.path(),
            &to,
            || Ok(lab.clone()),
            ByKind::Person,
            unread,
            NOW,
            false,
        )
        .expect_err("another Mac's number");
        assert_eq!(why.to_string(), format!("m20 is {}", hold.phrase()));
        assert_eq!(
            file::read(root.path()).expect("reads"),
            Some(before.clone()),
            "neither the number nor the nickname changed"
        );
    }
    let set = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("free with nothing held");
    assert_eq!(set.name.title(), "m20 Heron");
}

/// A move off the number the policy dials this Mac's vault server by is refused while the macOS
/// names follow that number: the roster label until a number is chosen, then the chosen one. A
/// rename that keeps it, or a server the policy names by another number, is not.
#[test]
fn a_move_off_the_policys_server_name_is_refused() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m8"), never, NOW).expect("a seeded name");
    let serves = |label: u32, server: u32| LabLabels {
        own: BTreeSet::from([num(label)]),
        label: Some(num(label)),
        servers: BTreeSet::from([num(server)]),
        ..LabLabels::default()
    };
    let to = |n: u32| NameTo {
        num: num(n),
        nick: None,
    };
    let why = set_name(
        root.path(),
        &to(87),
        || Ok(serves(8, 8)),
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect_err("the server's name");
    assert!(why.to_string().contains("dial as m8.local"), "{why}");
    assert_eq!(file::read(root.path()).expect("reads"), Some(before));
    // Negative control: a server the policy names by another number lets the move through.
    set_name(
        root.path(),
        &to(87),
        || Ok(serves(8, 9)),
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("this Mac serves no one by m8");
    // Chosen, the macOS names follow m87: a policy naming m8 no longer dials this Mac by it.
    set_name(
        root.path(),
        &to(88),
        || Ok(serves(8, 8)),
        ByKind::Person,
        unread,
        NOW + 1,
        false,
    )
    .expect("m87 is what the macOS names follow");
    let why = set_name(
        root.path(),
        &to(89),
        || Ok(serves(8, 88)),
        ByKind::Person,
        unread,
        NOW + 2,
        false,
    )
    .expect_err("the chosen number is the server's name");
    assert!(why.to_string().contains("dial as m88.local"), "{why}");
    set_name(
        root.path(),
        &to(88),
        || Ok(serves(8, 88)),
        ByKind::Person,
        unread,
        NOW + 3,
        false,
    )
    .expect("staying on it is no move");
}

/// A request that keeps the stored number, a nickname typed with it, reads no labels, so an
/// unreadable vault does not stop it; a number change needs them; and a request whose stored
/// number a newer signed floor moves under the lock is refused rather than checked unread.
#[test]
fn keeping_the_stored_number_reads_no_labels_unless_a_floor_moves_it() {
    let unreadable = || Err(io::Error::other("the vault is busy"));
    let root = state();
    ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let to = NameTo {
        num: num(7),
        nick: Some(Nickname::parse("Heron").expect("a nickname")),
    };
    let set = set_name(
        root.path(),
        &to,
        unreadable,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("a nickname edit reads no labels");
    assert_eq!(set.name.title(), "m7 Heron");
    let renumber = NameTo {
        num: num(70),
        nick: None,
    };
    let why = set_name(
        root.path(),
        &renumber,
        unreadable,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect_err("a number change needs its labels");
    assert!(why.to_string().contains("the vault is busy"), "{why}");
    let (moved, _, before, _) = edit_floor_fixture();
    let keep = NameTo {
        num: num(7),
        nick: None,
    };
    let why = set_name(
        moved.path(),
        &keep,
        unreadable,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect_err("the signed floor moved m7 to m9");
    assert!(why.to_string().contains("rename again"), "{why}");
    assert_eq!(file::read(moved.path()).expect("reads"), Some(before));
}

/// No vault read holds nothing; a home with no vault set up holds nothing either.
#[test]
fn lab_labels_with_no_vault_hold_nothing() {
    let root = state();
    assert_eq!(lab_labels(None).expect("reads"), LabLabels::default());
    let vault = aterm_secrets::client::Paths::locate_for_home(&root.path().join("home"));
    assert_eq!(
        lab_labels(Some(&vault)).expect("reads"),
        LabLabels::default()
    );
}

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

fn registry_name(n: u32, nick: Option<(&str, NickClass)>) -> RegistryNameRead {
    RegistryNameRead::Bound(Ok(RegistryName {
        num: num(n),
        nick: nick.map(|(word, class)| (Nickname::parse(word).expect("a nickname"), class)),
    }))
}

#[test]
fn a_bound_registry_name_precedes_legacy_books_and_roster_without_making_a_local_claim() {
    let root = state();
    let me = node_identity(root.path()).expect("an older local identity");
    write_private(root.path(), NAME_FILE, b"m7\n").expect("older legacy evidence");
    write_private(
        root.path(),
        PEERS_FILE,
        entry(&me, &record(&me, Some(9), 8, Some("Heron"))).as_bytes(),
    )
    .expect("older signed evidence");
    let (name, origin) = ensure_from(
        root.path(),
        || registry_name(86, Some(("MissBunny", NickClass::Chosen))),
        unread,
        |_| panic!("a supplied registry nickname needs no draw"),
        NOW,
    )
    .expect("uses the bound row");
    assert_eq!(origin, Origin::Registry);
    assert_eq!(name.title(), "m86 MissBunny");
    assert_eq!(
        (name.class, name.since, name.rev),
        (Class::Seeded, IMPLIED_SINCE, 0)
    );
    assert_eq!(name.migration, None);
    assert_eq!(name.by, None);
    assert_eq!(name.undo, None);
    assert!(name.was.is_empty());
    assert_eq!(file::read(root.path()).expect("reads"), Some(name));
}

#[test]
fn registry_nicknames_keep_their_spelling_and_automatic_or_chosen_provenance() {
    for class in [NickClass::Auto, NickClass::Chosen] {
        let root = state();
        let (name, origin) = ensure_from(
            root.path(),
            || registry_name(86, Some(("MissBunny", class))),
            unread,
            never,
            NOW,
        )
        .expect("a typed nickname");
        assert_eq!(origin, Origin::Registry);
        assert_eq!(name.nick.as_str(), "MissBunny");
        assert_eq!(name.nick_class, class);
        assert_eq!((name.num, name.class), (Some(num(86)), Class::Seeded));
    }
}

#[test]
fn a_null_registry_nickname_draws_a_local_automatic_noun_without_reading_legacy_sources() {
    use std::cell::Cell;
    let root = state();
    let draws = Cell::new(0);
    let (name, origin) = ensure_from(
        root.path(),
        || registry_name(86, None),
        unread,
        |_| {
            draws.set(draws.get() + 1);
            false
        },
        NOW,
    )
    .expect("draws the nickname only");
    assert_eq!((name.num, origin), (Some(num(86)), Origin::Registry));
    assert_eq!(name.nick_class, NickClass::Auto);
    assert!(draws.get() > 0);
    assert!(!name.nick.as_str().is_empty());
    assert!(
        read_node_identity(root.path())
            .expect("does not mint an identity")
            .is_none()
    );
}

#[test]
fn an_existing_local_name_reads_no_registry_roster_or_nickname_source() {
    let root = state();
    let winner = MachineName::unnumbered(Nickname::parse("Winner").expect("a nickname"), NOW);
    file::read_or_create(root.path(), || Ok(winner.clone())).expect("an existing local name");
    assert_eq!(
        ensure_from(
            root.path(),
            || panic!("an existing name does not ask the registry"),
            unread,
            |_| panic!("an existing name needs no draw"),
            NOW
        )
        .expect("keeps the local name"),
        (winner, Origin::Kept)
    );
}

#[test]
fn pending_or_invalid_bound_registry_names_refuse_first_creation_without_fallback() {
    for source in [
        RegistryNameRead::Pending,
        RegistryNameRead::Bound(Err("the label is conflicted".to_owned())),
    ] {
        let root = state();
        write_private(root.path(), NAME_FILE, b"m7\n").expect("a tempting legacy fallback");
        assert!(
            ensure_from(
                root.path(),
                || source,
                unread,
                |_| panic!("an unavailable registry name needs no draw"),
                NOW
            )
            .is_err()
        );
        assert!(
            file::read(root.path())
                .expect("reads no partial name")
                .is_none()
        );
    }
}

#[test]
fn an_unbound_registry_preserves_the_existing_fallback_order() {
    let root = state();
    write_private(root.path(), NAME_FILE, b"m7\n").expect("legacy evidence");
    let (name, origin) = ensure_from(
        root.path(),
        || RegistryNameRead::Unbound,
        no_roster,
        never,
        NOW,
    )
    .expect("uses legacy fallback");
    assert_eq!((name.num, origin), (Some(num(7)), Origin::Evidence));
    assert_eq!(name.migration, Some(Migration::Pending));
}

#[test]
fn a_concurrent_local_winner_overrides_registry_pending_or_invalid_results() {
    use aterm_uds::private_file::PrivateLock;
    use std::time::Duration;
    for source in [
        RegistryNameRead::Pending,
        RegistryNameRead::Bound(Err("the label is conflicted".to_owned())),
    ] {
        let root = state();
        let winner = MachineName::unnumbered(Nickname::parse("Winner").expect("a nickname"), NOW);
        let registry = || {
            let _lock = PrivateLock::take(
                root.path(),
                file::LOCK,
                Duration::ZERO,
                "registry preparation held the name lock",
            )
            .expect("preparation is outside the lock");
            write_private(
                root.path(),
                file::FILE,
                winner.to_toml().expect("the winner").as_bytes(),
            )
            .expect("a concurrent creator won");
            source
        };
        assert_eq!(
            ensure_from(root.path(), registry, unread, never, NOW)
                .expect("the valid winner stands"),
            (winner.clone(), Origin::Kept)
        );
        assert_eq!(file::read(root.path()).expect("reads"), Some(winner));
    }
}

#[test]
fn registry_names_remain_the_edit_base_despite_newer_old_own_book_records() {
    let root = state();
    let me = node_identity(root.path()).expect("this node's identity");
    let mut old = record(&me, Some(9), 9, Some("Heron"));
    old.claim.anchor = Some(aterm_types::machine_name::Anchor([0x42; 8]));
    old.claim.ephemeral = true;
    old.claim.sgen = 4;
    old.claim.top = 100;
    write_private(root.path(), PEERS_FILE, entry(&me, &old).as_bytes())
        .expect("a retired signed naming source");
    let (mirror, _) = ensure_from(
        root.path(),
        || registry_name(86, Some(("MissBunny", NickClass::Chosen))),
        unread,
        never,
        NOW,
    )
    .expect("the registry supplies the local mirror");
    let to = NickTo::Word(Nickname::parse("Bunny").expect("a nickname"));
    let preview = set_nick(root.path(), &to, ByKind::Person, unread, NOW + 1, true)
        .expect("previews from the local mirror");
    assert_eq!((preview.name.num, preview.name.rev), (Some(num(86)), 1));
    assert_eq!(
        file::read(root.path()).expect("reads"),
        Some(mirror.clone())
    );
    let nicked = set_nick(root.path(), &to, ByKind::Person, unread, NOW + 1, false)
        .expect("changes only the local nickname")
        .name;
    assert_eq!(nicked.title(), "m86 Bunny");
    assert_eq!(
        (nicked.class, nicked.since, nicked.act),
        (mirror.class, mirror.since, mirror.act),
        "a nickname edit does not strengthen its number"
    );
    assert_eq!(nicked.rev, 1);
    assert!(nicked.undo.is_none());

    let to = NameTo {
        num: num(87),
        nick: None,
    };
    let preview = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Person,
        unread,
        NOW + 2,
        true,
    )
    .expect("previews a local rename");
    assert_eq!(
        preview
            .name
            .undo
            .as_ref()
            .expect("the previous local claim")
            .num,
        Some(num(86))
    );
    assert_eq!(file::read(root.path()).expect("reads"), Some(nicked));
    let renamed = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Person,
        unread,
        NOW + 2,
        false,
    )
    .expect("renames the local mirror")
    .name;
    assert_eq!(renamed.title(), "m87 Bunny");
    assert_eq!(renamed.rev, 2);
    let undo = renamed.undo.expect("the actual previous local claim");
    assert_eq!(
        (undo.num, undo.class, undo.since),
        (Some(num(86)), Class::Seeded, IMPLIED_SINCE)
    );
    assert_eq!(
        renamed.was.iter().map(|was| was.num).collect::<Vec<_>>(),
        [num(86)]
    );
    assert_eq!((renamed.base, renamed.tgen, renamed.sgen), (None, 0, 0));
    assert_eq!(renamed.cap, None);
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
    assert_eq!(name.rev, 0, "the first local revision");
    assert_ne!(name.nick.as_str(), "otter", "a noun in view is not drawn");
    assert!(nouns::NOUNS.contains(&name.nick.as_str()));
    assert!(name.title().starts_with("unnumbered "), "{}", name.title());
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
    assert!(
        set_nick(
            root.path(),
            &NickTo::Word(Nickname::parse("Heron").unwrap()),
            ByKind::Person,
            unread,
            NOW,
            false
        )
        .is_err()
    );
}

#[test]
fn a_signed_name_arriving_after_preview_refuses_both_fields_at_write() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).unwrap();
    let nick = Nickname::parse("Blue Heron").unwrap();
    let preview = rename_preview(root.path(), no_labels, unread, NOW).unwrap();
    assert!(!preview.shared(&nick));
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).unwrap();
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                aterm_net::fleet_member::write_held(
                    root.path(),
                    aterm_net::fleet_member::Began::Here,
                    |_| {
                        write_private(
                            root.path(),
                            PEERS_FILE,
                            entry(&other, &record(&other, Some(86), 3, Some("BLUE HERON")))
                                .as_bytes(),
                        )
                    },
                )
                .unwrap()
                .unwrap();
            })
            .join()
            .unwrap();
    });
    let to = NameTo {
        num: num(8),
        nick: Some(nick),
    };
    for dry_run in [true, false] {
        assert!(
            set_name(
                root.path(),
                &to,
                no_labels,
                ByKind::Person,
                unread,
                NOW,
                dry_run
            )
            .is_err()
        );
        assert_eq!(file::read(root.path()).unwrap(), Some(before.clone()));
    }
}

#[test]
fn concurrent_combined_edits_leave_one_complete_choice() {
    let root = state();
    ensure(root.path(), label("m7"), never, NOW).unwrap();
    node_identity(root.path()).unwrap();
    let choices = [
        NameTo {
            num: num(8),
            nick: Some(Nickname::parse("Heron").unwrap()),
        },
        NameTo {
            num: num(9),
            nick: Some(Nickname::parse("Otter").unwrap()),
        },
    ];
    let start = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let threads: Vec<_> = choices
            .iter()
            .map(|to| {
                scope.spawn(|| {
                    start.wait();
                    set_name(
                        root.path(),
                        to,
                        no_labels,
                        ByKind::Person,
                        unread,
                        NOW,
                        false,
                    )
                    .unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    });
    let after = file::read(root.path()).unwrap().unwrap();
    assert!(
        choices
            .iter()
            .any(|to| after.num == Some(to.num) && Some(&after.nick) == to.nick.as_ref())
    );
    assert_eq!(after.rev, 2);
}

#[test]
fn name_uniqueness_uses_the_newest_verified_record_in_both_books() {
    let root = state();
    node_identity(root.path()).unwrap();
    ensure(root.path(), label("m7"), never, NOW).unwrap();
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).unwrap();
    write_private(
        root.path(),
        ISSUER_BOOK_FILE,
        entry(&other, &record(&other, Some(86), 3, Some("Blue Heron"))).as_bytes(),
    )
    .unwrap();
    write_private(
        root.path(),
        PEERS_FILE,
        entry(&other, &record(&other, Some(86), 4, Some("Otter"))).as_bytes(),
    )
    .unwrap();
    let preview = rename_preview(root.path(), no_labels, unread, NOW).unwrap();
    assert!(!preview.shared(&Nickname::parse("Blue Heron").unwrap()));
    assert!(preview.shared(&Nickname::parse("OTTER").unwrap()));
    set_nick(
        root.path(),
        &NickTo::Word(Nickname::parse("Blue Heron").unwrap()),
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .unwrap();
    assert!(
        set_nick(
            root.path(),
            &NickTo::Word(Nickname::parse("otter").unwrap()),
            ByKind::Person,
            unread,
            NOW,
            false
        )
        .is_err()
    );

    let signed = entry(&other, &record(&other, Some(86), 5, Some("Puffin")));
    let tampered = signed.replace("m9.local", "m10.local");
    assert_ne!(tampered, signed);
    write_private(root.path(), PEERS_FILE, tampered.as_bytes()).unwrap();
    let preview = rename_preview(root.path(), no_labels, unread, NOW).unwrap();
    assert!(!preview.shared(&Nickname::parse("Puffin").unwrap()));
    set_nick(
        root.path(),
        &NickTo::Word(Nickname::parse("Puffin").unwrap()),
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .unwrap();
}

#[test]
fn an_exhausted_default_name_pool_leaves_first_creation_unwritten() {
    let root = state();
    let why = ensure(root.path(), || Ok(None), |_| true, NOW).unwrap_err();
    assert_eq!(why.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(file::read(root.path()).unwrap(), None);
}

#[test]
fn an_exhausted_random_name_pool_cannot_save_a_duplicate() {
    let root = state();
    node_identity(root.path()).unwrap();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).unwrap();
    let mut books = [String::new(), String::new()];
    for (index, noun) in nouns::NOUNS.iter().enumerate() {
        let number = u32::try_from(index + 1).unwrap();
        let other = NodeIdentity::from_seed(&format!("{number:032x}"), &[0x22; 32]).unwrap();
        books[index % books.len()]
            .push_str(&entry(&other, &record(&other, Some(number), 1, Some(noun))));
    }
    for (file, book) in [PEERS_FILE, ISSUER_BOOK_FILE].into_iter().zip(books) {
        write_private(root.path(), file, book.as_bytes()).unwrap();
    }
    let why = set_nick(
        root.path(),
        &NickTo::Random,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .unwrap_err();
    assert!(why.to_string().contains("already used"), "{why}");
    assert_eq!(file::read(root.path()).unwrap(), Some(before));
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
    let mut stale = record(&other, Some(40), 2, Some("Puffin"));
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
        let preview = rename_preview(root.path(), no_labels, unread, NOW).unwrap();
        assert!(preview.shared(&Nickname::parse("OTTER").unwrap()));
        assert!(!preview.shared(&Nickname::parse("Puffin").unwrap()));
        assert!(
            set_name(
                root.path(),
                &NameTo {
                    num: num(8),
                    nick: Some(Nickname::parse("otter").unwrap())
                },
                no_labels,
                ByKind::Person,
                unread,
                NOW,
                false,
            )
            .is_err()
        );
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
    assert!(same.changed);
    assert_eq!(same.name.class, Class::Chosen);
    let node = node_of(node_identity(root.path()).expect("our identity").node()).expect("a node");
    let mut view = rename_books(root.path(), NOW)
        .expect("reads the signed rival")
        .view;
    view.insert(node, Entry::Record(current_record(&same.name)));
    assert_eq!(
        settle(&view).standings.get(&node),
        Some(&aterm_types::machine_name::Standing::Holds(num(7))),
        "choosing the current seeded number must preserve its winning claim"
    );
    assert_eq!(same.name.since, before.since);
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
        (same.name.class, same.name.since, same.name.act)
    );
    assert_eq!(
        (nick.name.undo, nick.name.prev, nick.name.was),
        (same.name.undo, same.name.prev, same.name.was)
    );
}

#[test]
fn an_upgraded_machine_keeps_its_older_name_pending_migration() {
    let root = state();
    write_private(root.path(), NAME_FILE, b"m7\n").expect("writes");
    let (name, origin) = ensure(root.path(), no_roster, never, NOW).expect("ensures");
    assert_eq!(origin, Origin::Evidence);
    assert_eq!(
        (name.num, name.class, name.since, name.migration),
        (
            Some(num(7)),
            Class::Seeded,
            IMPLIED_SINCE,
            Some(Migration::Pending)
        ),
        "the legacy fleet name when no roster label exists"
    );
    assert_eq!(name.nick_class, NickClass::Auto, "no word to keep: drawn");

    let root = state();
    write_private(root.path(), NAME_FILE, b"studio\n").expect("writes");
    let (name, origin) = ensure(root.path(), label("m12"), never, NOW).expect("ensures");
    assert_eq!(origin, Origin::Evidence);
    assert_eq!(
        name.title(),
        "m12 studio",
        "the roster's NAME, the fleet name's word"
    );
    assert_eq!(name.nick_class, NickClass::Chosen);
    assert_eq!(name.migration, Some(Migration::Pending));
}

#[test]
fn the_roster_precedes_legacy_names_and_old_own_signed_records() {
    let root = state();
    let me = node_identity(root.path()).expect("a node key");
    write_private(root.path(), NAME_FILE, b"m7\n").expect("the old fleet name");
    write_private(
        root.path(),
        PEERS_FILE,
        entry(&me, &record(&me, Some(9), 3, None)).as_bytes(),
    )
    .expect("writes the book");
    let (name, origin) = ensure(root.path(), label("m12"), never, NOW).expect("ensures");
    assert_eq!((name.num, origin), (Some(num(12)), Origin::Evidence));
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
    let taken = [
        Nickname::parse("Blue Heron").unwrap(),
        Nickname::parse("Élan").unwrap(),
    ];
    assert!(in_view(&taken, " BLUE\t heron "));
    assert!(in_view(&taken, "ÉLAN"));
}

#[test]
fn a_nickname_change_is_chosen_raises_rev_and_keeps_the_claim() {
    let heron = || Nickname::parse("heron").expect("a nickname");
    let held = MachineName::unnumbered(heron(), NOW);
    let chosen = renicked(&held, heron(), None).expect("a drawn noun chosen is a change");
    assert_eq!(
        (chosen.nick_class, chosen.rev),
        (NickClass::Chosen, 1),
        "every changed local nickname advances its revision"
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
        InFleet::Paused(String::new()),
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
            ("paused", "fleet paused"),
            ("unread", "fleet not read"),
        ]
    );
}

/// A retained key is active only under admitted custody; an explicit take restores it.
/// Under a private state root (a test binary) no vault is read: `$HOME`'s is the person's.
#[test]
fn a_fleet_key_reports_its_custody_without_discarding_the_key() {
    let root = state();
    let vault = aterm_secrets::client::Paths::locate_for_home(&root.path().join("home"));
    assert_eq!(InFleet::read(root.path(), Some(&vault)), InFleet::Out);
    let key = format!("{}\n", "ab".repeat(32));
    write_private(root.path(), aterm_net::fleet::KEY_FILE, key.as_bytes()).expect("a fleet key");
    assert!(matches!(
        InFleet::read(root.path(), Some(&vault)),
        InFleet::Paused(_)
    ));
    assert_eq!(InFleet::read(root.path(), None), InFleet::In);
    crate::fleet_vault::take_fixture(root.path(), Some(&vault)).unwrap();
    assert_eq!(InFleet::read(root.path(), Some(&vault)), InFleet::In);
    aterm_secrets::fsx::ensure_private_dir(&vault.dir).unwrap();
    let source = vault.dir.join("source.toml");
    for name in ["first", "second"] {
        aterm_secrets::fsx::write_atomic(
            &source,
            format!(
                "url = \"http://127.0.0.1:9/v1/{name}\"\nname = \"{name}\"\nsigner = \"{}\"\nhost = \"m22\"\n",
                aterm_secrets::b64::encode_padded(&[7; 32]),
            ).as_bytes(),
            0o600,
        ).unwrap();
        assert!(matches!(
            InFleet::read(root.path(), Some(&vault)),
            InFleet::Paused(_)
        ));
        assert_eq!(
            std::fs::read(root.path().join(aterm_net::fleet::KEY_FILE)).unwrap(),
            key.as_bytes()
        );
        crate::fleet_vault::take_fixture(root.path(), Some(&vault)).unwrap();
        assert_eq!(InFleet::read(root.path(), Some(&vault)), InFleet::In);
    }
    assert!(super::standing_vault().is_none(), "a test reads no vault");
}

/// The vault's labels on today's lab (§11.86): a number its roster or log gives another Mac is
/// refused with what holds it, changing nothing; the same number with no labels read renames,
/// which shows the labels are what refuse.
#[test]
fn a_number_the_vault_gives_another_mac_is_refused_with_its_holder() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let to = NameTo {
        num: num(20),
        nick: Some(Nickname::parse("Heron").expect("a nickname")),
    };
    for hold in [LabelHold::Label, LabelHold::Seat, LabelHold::Denied] {
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

/// A newly explicit choice of a seeded number needs current labels. Once chosen, keeping
/// the number for a nickname edit reads no labels, while a number change still needs them.
#[test]
fn keeping_the_stored_number_reads_no_labels_but_a_number_change_requires_them() {
    let unreadable = || Err(io::Error::other("the vault is busy"));
    let root = state();
    let (seeded, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let same = NameTo {
        num: num(7),
        nick: None,
    };
    assert!(
        set_name(
            root.path(),
            &same,
            unreadable,
            ByKind::Person,
            unread,
            NOW,
            false
        )
        .is_err(),
        "a new explicit choice requires current labels even when its number was seeded"
    );
    assert_eq!(file::read(root.path()).unwrap(), Some(seeded.clone()));
    let held_elsewhere = || {
        Ok(LabLabels {
            held: BTreeMap::from([(num(7), client::LabelHold::Seat)]),
            ..LabLabels::default()
        })
    };
    assert!(
        set_name(
            root.path(),
            &same,
            held_elsewhere,
            ByKind::Person,
            unread,
            NOW,
            false
        )
        .is_err(),
        "another Mac's fresh seat on m7 prevents turning a stale seeded number into a chosen claim"
    );
    assert_eq!(file::read(root.path()).unwrap(), Some(seeded));
    let selected = set_name(
        root.path(),
        &same,
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("explicitly selects the seeded number");
    assert_eq!(selected.name.class, Class::Chosen);
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

/// Rename This Mac…'s live line judges each number from one read as [`set_name`] does, in its
/// words: a number another signed node or the vault holds is refused, a free one passes and this
/// Mac's chosen number is `Same`; unread labels refuse each new explicit claim.
#[test]
fn a_preview_judges_each_number_in_the_words_the_set_refuses_it() {
    let root = state();
    node_identity(root.path()).expect("our identity");
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
    ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let before = set_name(
        root.path(),
        &NameTo {
            num: num(7),
            nick: None,
        },
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("a chosen number")
    .name;
    let theirs = record(&other, Some(86), 3, Some("Otter"));
    write_private(root.path(), PEERS_FILE, entry(&other, &theirs).as_bytes())
        .expect("writes their signed claim");
    let lab = LabLabels {
        held: BTreeMap::from([(num(20), LabelHold::Label)]),
        ..LabLabels::default()
    };
    let preview = rename_preview(root.path(), || Ok(lab.clone()), unread, NOW).expect("a read");
    assert_eq!(preview.name(), &before);
    for n in [86, 20] {
        let to = NameTo {
            num: num(n),
            nick: None,
        };
        let refused = set_name(
            root.path(),
            &to,
            || Ok(lab.clone()),
            ByKind::Person,
            unread,
            NOW,
            true,
        )
        .expect_err("held");
        assert_eq!(preview.judge(num(n)), Err(refused.to_string()), "m{n}");
    }
    assert_eq!(preview.judge(num(7)), Ok(Verdict::Same));
    assert_eq!(preview.judge(num(87)), Ok(Verdict::Free));
    assert!(preview.shared(&Nickname::parse("otter").expect("a nickname")));
    assert!(!preview.shared(&Nickname::parse("Kestrel").expect("a nickname")));
    let unlabelled = rename_preview(
        root.path(),
        || Err(io::Error::other("the vault's labels are unread: busy")),
        unread,
        NOW,
    )
    .expect("a read");
    assert_eq!(
        unlabelled.judge(num(87)),
        Err("the vault's labels are unread: busy".to_owned())
    );
    assert_eq!(
        unlabelled.unchecked(num(87)),
        Some("the vault's labels are unread: busy")
    );
    assert_eq!(unlabelled.judge(num(7)), Ok(Verdict::Same));
    assert_eq!(
        unlabelled.unchecked(num(7)),
        None,
        "a kept number needs no labels"
    );
    assert_eq!(preview.unchecked(num(87)), None, "the labels were read");
    assert_eq!(
        file::read(root.path()).expect("reads"),
        Some(before),
        "a read changes no name"
    );
    // Keeping a chosen number needs no labels; a move still checks the policy's
    // server name, which now follows the chosen number rather than the roster label.
    let serves = LabLabels {
        own: BTreeSet::from([num(8)]),
        label: Some(num(8)),
        servers: BTreeSet::from([num(7)]),
        ..LabLabels::default()
    };
    let served = rename_preview(root.path(), || Ok(serves.clone()), unread, NOW).expect("a read");
    let keep = NameTo {
        num: num(7),
        nick: Some(Nickname::parse("Kestrel").expect("a nickname")),
    };
    set_name(
        root.path(),
        &keep,
        || Ok(serves.clone()),
        ByKind::Person,
        unread,
        NOW,
        true,
    )
    .expect("a kept number reads no labels");
    assert_eq!(served.judge(num(7)), Ok(Verdict::Same));
    let refused = set_name(
        root.path(),
        &NameTo {
            num: num(87),
            nick: None,
        },
        || Ok(serves.clone()),
        ByKind::Person,
        unread,
        NOW,
        true,
    )
    .expect_err("a move off the server's name");
    assert_eq!(served.judge(num(87)), Err(refused.to_string()));
    // A number a migration waits on holds no claim: the read says so and judges it as a move, and
    // the set then makes the claim.
    assert!(!preview.waits());
    file::replace(root.path(), |name| {
        name.migration = Some(Migration::Waiting);
        Ok(())
    })
    .expect("a waiting number");
    let waiting = rename_preview(root.path(), no_labels, unread, NOW).expect("a read");
    assert!(waiting.waits());
    assert!(waiting.judge(num(7)).is_ok());
    let unlabelled =
        rename_preview(root.path(), || Err(io::Error::other("busy")), unread, NOW).expect("a read");
    assert_eq!(unlabelled.judge(num(7)), Err("busy".to_owned()));
    let claimed = set_name(
        root.path(),
        &NameTo {
            num: num(7),
            nick: None,
        },
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("the shown number is claimed");
    assert_eq!(
        (claimed.name.num, claimed.name.migration),
        (Some(num(7)), None)
    );
}

/// A number that moves the name the edit starts from is judged only with the vault's labels read:
/// with none it is refused, so a stored number that moved under the set's locks after its unlocked
/// read decided it kept the number is never renamed past the vault or the server rule; the number
/// held, or a read set of labels, passes.
#[test]
fn a_move_judged_without_the_vault_s_labels_is_refused() {
    let root = state();
    node_identity(root.path()).expect("our identity");
    ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let held = set_name(
        root.path(),
        &NameTo {
            num: num(7),
            nick: None,
        },
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("a chosen number")
    .name;
    let node = node_of(node_identity(root.path()).expect("our identity").node()).expect("a node");
    let view = BTreeMap::from([(node, Entry::Record(current_record(&held)))]);
    let settled = settle(&view);
    let fleet = Fleet {
        view: &view,
        settled: &settled,
        top: 7,
        kept: &[],
        labels: &BTreeMap::new(),
    };
    let refused = judge(num(87), &held, None, &fleet, node).expect_err("unread labels");
    assert!(refused.contains("rename again"), "{refused}");
    assert_eq!(
        judge(num(87), &held, Some(&LabLabels::default()), &fleet, node),
        Ok(Verdict::Free)
    );
    assert_eq!(judge(num(7), &held, None, &fleet, node), Ok(Verdict::Same));
    let mut waiting = held;
    waiting.migration = Some(Migration::Waiting);
    let refused = judge(num(7), &waiting, None, &fleet, node).expect_err("a waiting number moves");
    assert!(refused.contains("rename again"), "{refused}");
}

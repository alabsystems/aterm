// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Late 2026, well past [`FLOOR_MS`].
const NOW: u64 = FLOOR_MS + 300 * 86_400_000;

fn identity(i: u8) -> NodeIdentity {
    NodeIdentity::from_seed(&format!("{i:032x}"), &[i; 32]).expect("an identity")
}

fn id(me: &NodeIdentity) -> NodeId {
    node_of(me.node()).expect("a node id")
}

fn num(n: u32) -> Number {
    Number::new(n).expect("a number")
}

fn plain(me: &NodeIdentity) -> NameRecord {
    NameRecord {
        claim: Record {
            num: Some(num(8)),
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
            top: 8,
            cap: None,
        },
        rev: 1,
        eseq: 5,
        boot: None,
        nick: None,
        nick_class: NickClass::Auto,
        by: By {
            node: id(me),
            kind: ByKind::Person,
        },
    }
}

/// Every field present at its widest: 16-byte node ids, eight `was` entries with both flags,
/// a 64-byte nickname.
fn widest(me: &NodeIdentity) -> NameRecord {
    let nick = format!("a{}", "鷺".repeat(21));
    assert_eq!(nick.len(), Nickname::MAX_BYTES);
    let mut record = plain(me);
    record.claim = Record {
        num: Some(num(999_999)),
        class: Class::Seeded,
        since: u64::MAX,
        base: Some(id(&identity(9))),
        tgen: u32::MAX - 1,
        sgen: u32::MAX - 1,
        act: u64::MAX,
        anchor: Some(Anchor([0xA5; 8])),
        prev: Some(num(999_998)),
        waits: true,
        ephemeral: true,
        was: (0..8)
            .map(|i| Was {
                num: num(999_990 + i),
                retired: true,
                lost: true,
            })
            .collect(),
        top: u32::MAX,
        cap: Some(Cap {
            value: u32::MAX,
            since: u64::MAX,
        }),
    };
    record.rev = u64::MAX;
    record.eseq = u64::MAX;
    record.boot = Some([0x5A; 8]);
    record.nick = Some(Nickname::parse(&nick).expect("a nickname"));
    record.nick_class = NickClass::Chosen;
    record.by.kind = ByKind::Auto;
    record
}

fn carried(words: &str) -> Carried {
    let get = |key: &str| {
        words
            .split(' ')
            .find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
    };
    Carried::of_words(get("mname"), get("mname_sig")).expect("carried words")
}

fn words(me: &NodeIdentity, record: &NameRecord, digest: [u8; 8]) -> String {
    record.sign(me, digest).expect("signs").words()
}

fn accept_at(
    me: &NodeIdentity,
    record: &NameRecord,
    signed: [u8; 8],
    reader: [u8; 8],
    source: Source,
) -> Result<Accepted, Refused> {
    accept(
        &record.sign(me, signed).expect("signs"),
        (me.node(), &me.key()),
        reader,
        source,
        None,
        NOW,
    )
}

#[test]
fn every_record_round_trips_through_its_one_spelling() {
    let me = identity(1);
    for record in [plain(&me), widest(&me)] {
        let bytes = record.encode();
        assert_eq!(NameRecord::decode(&bytes), Ok(record.clone()));
    }
    let mesh = NodeIdentity::from_seed("n-0123456789abcdef", &[3; 32]).expect("a mesh identity");
    let record = plain(&mesh);
    assert_eq!(record.by.node.as_bytes().len(), 8);
    assert_eq!(node_word(record.by.node), "n-0123456789abcdef");
    assert_eq!(NameRecord::decode(&record.encode()), Ok(record));
}

#[test]
fn the_widest_record_is_the_measured_body_and_words() {
    let me = identity(1);
    let record = widest(&me);
    assert_eq!(record.encode().len(), BODY_MAX);
    let words = words(&me, &record, ZERO_DIGEST);
    assert_eq!(words.len(), WORDS_MAX);
    assert_eq!(WORDS_MAX, 433, "§3.2's measure");
}

#[test]
fn bytes_the_encoder_would_not_write_are_no_record() {
    let me = identity(1);
    let good = widest(&me).encode();
    assert!(
        NameRecord::decode(&good).is_ok(),
        "control: the bytes decode"
    );
    let with = |at: usize, byte: u8| {
        let mut bytes = good.clone();
        bytes[at] = byte;
        NameRecord::decode(&bytes)
    };
    assert_eq!(with(0, 2), Err(Malformed::Version(2)));
    assert_eq!(
        with(1, good[1] | 0x80),
        Err(Malformed::Field("reserved bit"))
    );
    assert_eq!(
        with(2, good[2] | 0x80),
        Err(Malformed::Field("reserved bit"))
    );
    assert_eq!(with(1, good[1] | 0b11), Err(Malformed::Field("class")));
    let mut unrevved = good.clone();
    // v, flags, presence, num, since: then rev.
    unrevved[15..23].fill(0);
    assert_eq!(
        NameRecord::decode(&unrevved),
        Err(Malformed::Field("rev")),
        "rev 0 is no record's"
    );
    assert_eq!(with(1, good[1] | 0b0110_0000), Err(Malformed::Field("by")));
    let mut trailing = good.clone();
    trailing.push(0);
    assert_eq!(NameRecord::decode(&trailing), Err(Malformed::Trailing));
    assert_eq!(
        NameRecord::decode(&good[..good.len() - 1]),
        Err(Malformed::Short)
    );

    let mut zero = plain(&me);
    let bytes = zero.encode();
    // num sits at 3..7 in a record that carries one.
    let mut unnumbered = bytes.clone();
    unnumbered[3..7].copy_from_slice(&0u32.to_be_bytes());
    assert_eq!(
        NameRecord::decode(&unnumbered),
        Err(Malformed::Field("num"))
    );
    let mut past = bytes.clone();
    past[3..7].copy_from_slice(&1_000_000u32.to_be_bytes());
    assert_eq!(NameRecord::decode(&past), Err(Malformed::Field("num")));

    zero.claim.was = vec![Was {
        num: num(7),
        retired: false,
        lost: false,
    }];
    let bytes = zero.encode();
    let at = bytes.len() - 1 - 16 - 4;
    let mut flagged = bytes.clone();
    flagged[at] |= 0x08;
    assert_eq!(NameRecord::decode(&flagged), Err(Malformed::Field("was")));
    let mut nine = bytes.clone();
    nine[at - 1] = 9;
    assert_eq!(NameRecord::decode(&nine), Err(Malformed::Field("was")));
}

#[test]
fn a_nickname_outside_its_grammar_is_no_record() {
    let me = identity(1);
    let mut record = plain(&me);
    record.nick = Some(Nickname::parse("heron").expect("a nickname"));
    let good = record.encode();
    assert!(NameRecord::decode(&good).is_ok(), "control");
    let at = good.len() - 17 - 5;
    for bad in [*b"1eron", *b"her!n"] {
        let mut bytes = good.clone();
        bytes[at..at + 5].copy_from_slice(&bad);
        assert_eq!(NameRecord::decode(&bytes), Err(Malformed::Field("nick")));
    }
}

#[test]
fn a_record_verifies_only_under_its_node_key_and_fleet() {
    let me = identity(1);
    let seed = [7; 16];
    let digest = seed_digest(&seed);
    let record = plain(&me);
    let words = words(&me, &record, digest);
    let held = carried(&words);
    let at = |node: &str, key: &NodeKey, reader: [u8; 8]| {
        accept(&held, (node, key), reader, Source::Entry, None, NOW)
    };
    assert_eq!(
        at(me.node(), &me.key(), digest),
        Ok(Accepted::Record {
            record: record.clone(),
            digest
        })
    );
    let other = identity(2);
    assert_eq!(
        at(other.node(), &me.key(), digest),
        Err(Refused::Signature),
        "another node's entry"
    );
    assert_eq!(
        at(me.node(), &other.key(), digest),
        Err(Refused::Signature),
        "another key"
    );
    assert_eq!(
        at(me.node(), &me.key(), seed_digest(&[8; 16])),
        Err(Refused::Signature),
        "another fleet's seed"
    );
    let mut bytes = held.bytes().to_vec();
    bytes[3..7].copy_from_slice(&9u32.to_be_bytes());
    let forged = Carried::new(&bytes, *held.sig()).expect("fits");
    assert_eq!(
        accept(
            &forged,
            (me.node(), &me.key()),
            digest,
            Source::Entry,
            None,
            NOW
        ),
        Err(Refused::Signature),
        "a changed NAME"
    );
}

#[test]
fn an_unseeded_record_stands_only_in_a_book_entry() {
    let me = identity(1);
    let record = plain(&me);
    let seeded = seed_digest(&[7; 16]);
    for reader in [ZERO_DIGEST, seeded] {
        assert_eq!(
            accept_at(&me, &record, ZERO_DIGEST, reader, Source::Entry),
            Ok(Accepted::Record {
                record: record.clone(),
                digest: ZERO_DIGEST
            })
        );
        assert_eq!(
            accept_at(&me, &record, ZERO_DIGEST, reader, Source::Copy),
            Err(Refused::UnseededCopy)
        );
    }
    assert_eq!(
        accept_at(&me, &record, seeded, seeded, Source::Copy),
        Ok(Accepted::Record {
            record,
            digest: seeded
        }),
        "control: a seeded copy stands"
    );
}

#[test]
fn a_replay_below_the_highest_accepted_drops() {
    let me = identity(1);
    let record = plain(&me);
    let words = words(&me, &record, ZERO_DIGEST);
    let held = carried(&words);
    let at = |highest| {
        accept(
            &held,
            (me.node(), &me.key()),
            ZERO_DIGEST,
            Source::Entry,
            Some(highest),
            NOW,
        )
    };
    assert!(at((5, 1)).is_ok(), "the same (eseq, rev) stands");
    assert!(at((4, 9)).is_ok(), "eseq orders first");
    assert_eq!(at((5, 2)), Err(Refused::Replay { held: (5, 2) }));
    assert_eq!(at((6, 0)), Err(Refused::Replay { held: (6, 0) }));
}

#[test]
fn since_and_act_stand_between_the_floor_and_the_readers_clock() {
    let me = identity(1);
    let at = |edit: &dyn Fn(&mut NameRecord)| {
        let mut record = plain(&me);
        edit(&mut record);
        accept_at(&me, &record, ZERO_DIGEST, ZERO_DIGEST, Source::Entry)
    };
    assert!(matches!(at(&|_| {}), Ok(Accepted::Record { .. })));
    assert_eq!(
        at(&|r| r.claim.since = FLOOR_MS - 1),
        Err(Refused::BeforeFloor)
    );
    assert_eq!(
        at(&|r| r.claim.act = FLOOR_MS - 1),
        Err(Refused::BeforeFloor)
    );
    assert!(matches!(
        at(&|r| r.claim.since = NOW + SKEW_MS),
        Ok(Accepted::Record { .. })
    ));
    assert!(matches!(
        at(&|r| r.claim.since = NOW + SKEW_MS + 1),
        Ok(Accepted::Ahead(_))
    ));
    assert!(matches!(
        at(&|r| r.claim.act = NOW + SKEW_MS + 1),
        Ok(Accepted::Ahead(_))
    ));

    // An evidence rank stands on a seeded record, a take (a take-back of a seeded chain
    // included) and a succession, nowhere else.
    let ranked = |class: Class, base: Option<NodeId>, tgen: u32, sgen: u32| {
        at(&|r| {
            r.claim.since = 3;
            r.claim.class = class;
            r.claim.base = base;
            r.claim.tgen = tgen;
            r.claim.sgen = sgen;
        })
    };
    let other = id(&identity(2));
    for (class, base, tgen, sgen, why) in [
        (Class::Seeded, None, 0, 0, "a migrated claim"),
        (Class::Chosen, Some(other), 1, 0, "a take of one"),
        (Class::Chosen, Some(id(&me)), 2, 0, "its origin's take-back"),
        (Class::Seeded, None, 0, 1, "a succession"),
        (Class::Chosen, None, 0, 1, "a succession that renamed"),
    ] {
        assert!(
            matches!(ranked(class, base, tgen, sgen), Ok(Accepted::Record { .. })),
            "{why}"
        );
    }
    assert_eq!(ranked(Class::Chosen, None, 0, 0), Err(Refused::BeforeFloor));
    assert_eq!(
        ranked(Class::Auto, Some(id(&me)), 0, 0),
        Err(Refused::BeforeFloor)
    );
    // A cap's since stands where `act` does.
    let capped = |since: u64| {
        at(&|r| {
            r.claim.cap = Some(Cap { value: 40, since });
        })
    };
    assert!(matches!(capped(NOW), Ok(Accepted::Record { .. })));
    assert_eq!(capped(FLOOR_MS - 1), Err(Refused::BeforeFloor));
    assert!(matches!(capped(NOW + SKEW_MS + 1), Ok(Accepted::Ahead(_))));
    assert_eq!(
        at(&|r| {
            r.claim.since = EVIDENCE_RANK_MAX + 1;
            r.claim.class = Class::Seeded;
        }),
        Err(Refused::BeforeFloor),
        "past the last rank"
    );
}

#[test]
fn generations_and_tops_past_their_bounds_drop() {
    let me = identity(1);
    let at = |edit: &dyn Fn(&mut NameRecord)| {
        let mut record = plain(&me);
        edit(&mut record);
        accept_at(&me, &record, ZERO_DIGEST, ZERO_DIGEST, Source::Entry)
    };
    assert_eq!(
        at(&|r| r.claim.tgen = u32::MAX),
        Err(Refused::Bound("tgen"))
    );
    assert_eq!(
        at(&|r| r.claim.sgen = u32::MAX),
        Err(Refused::Bound("sgen"))
    );
    assert_eq!(
        at(&|r| r.claim.top = Number::MAX + 1),
        Err(Refused::Bound("top"))
    );
    assert_eq!(
        at(&|r| {
            r.claim.cap = Some(Cap {
                value: Number::MAX + 1,
                since: NOW,
            });
        }),
        Err(Refused::Bound("cap"))
    );
    assert!(
        matches!(
            at(&|r| {
                r.claim.tgen = u32::MAX - 1;
                r.claim.top = Number::MAX;
            }),
            Ok(Accepted::Record { .. })
        ),
        "control: the last values that stand"
    );
}

#[test]
fn malformed_words_drop_only_the_record() {
    let me = identity(1);
    let words = words(&me, &plain(&me), ZERO_DIGEST);
    let held = carried(&words);
    let b64 = {
        let mut out = String::new();
        aterm_codec::base64::encode_url_into(&mut out, held.bytes()).expect("encodes");
        out
    };
    let sig = aterm_codec::hex::encode_lower(held.sig());
    assert_eq!(Carried::of_words(Some(&b64), Some(&sig)), Some(held));
    assert_eq!(Carried::of_words(Some(&b64), None), None);
    assert_eq!(Carried::of_words(None, Some(&sig)), None);
    assert_eq!(
        Carried::of_words(Some(&format!("{b64}=")), Some(&sig)),
        None
    );
    assert_eq!(
        Carried::of_words(Some(&b64), Some(&sig.to_uppercase())),
        None
    );
    assert_eq!(Carried::of_words(Some(&b64), Some(&sig[2..])), None);
    let long = {
        let mut out = String::new();
        aterm_codec::base64::encode_url_into(&mut out, &[0; BODY_MAX + 1]).expect("encodes");
        out
    };
    assert_eq!(Carried::of_words(Some(&long), Some(&sig)), None);
}

#[test]
fn the_anchor_is_one_per_machine_and_fleet() {
    let seed = [7; 16];
    let key = anchor_key(&seed);
    let a = anchor(&key, "macos", "1F2E3D4C-0000-4000-8000-00000000AB12");
    assert_eq!(
        a,
        anchor(&key, "macos", " 1f2e3d4c-0000-4000-8000-00000000ab12\n"),
        "one spelling of one platform id"
    );
    assert_ne!(
        a,
        anchor(&key, "linux", "1F2E3D4C-0000-4000-8000-00000000AB12")
    );
    assert_ne!(
        a,
        anchor(
            &anchor_key(&[8; 16]),
            "macos",
            "1F2E3D4C-0000-4000-8000-00000000AB12"
        ),
        "another fleet links nothing"
    );
    assert_ne!(boot(&key, "boot-a"), boot(&key, "boot-b"));
    assert_ne!(seed_digest(&seed), seed_digest(&[8; 16]));
    assert_ne!(seed_digest(&seed), ZERO_DIGEST);
}

#[test]
fn node_ids_spell_back_to_the_books_words() {
    for word in ["0123456789abcdef0123456789abcdef", "n-0123456789abcdef"] {
        assert_eq!(node_of(word).map(node_word).as_deref(), Some(word));
    }
    for word in [
        "0123456789ABCDEF0123456789ABCDEF",
        "n-0123",
        "node-0123456789abcdef",
    ] {
        assert_eq!(node_of(word), None, "{word}");
    }
}

/// `me`'s own signed entry at `host`, named `m8`, carrying `record` signed under the zero digest.
fn entry(me: &NodeIdentity, host: &str, record: Option<&NameRecord>) -> crate::fleet_member::Peer {
    let mut peer = crate::fleet_member::Peer::own(me, (host, 7485, "0.160.0"), NOW)
        .with_primary_name(me, Some("m8"));
    peer.signed = peer
        .signed
        .map(|signed| signed.with_record(record.and_then(|r| r.sign(me, ZERO_DIGEST))));
    peer
}

#[test]
fn an_entry_carries_its_record_beside_its_signature() {
    let me = identity(1);
    let record = plain(&me);
    let peer = entry(&me, "m8.local", Some(&record));
    let line = peer.line();
    assert!(line.contains(" mname="), "{line}");
    let back = crate::fleet_member::Peer::parse(&line).expect("the line parses");
    assert_eq!(back, peer);
    assert!(
        back.signature_holds(),
        "the record leaves the entry signature whole"
    );
    let carried = back
        .signed
        .as_ref()
        .and_then(|signed| signed.record())
        .expect("the record rides along");
    assert_eq!(
        accept(
            carried,
            (me.node(), &me.key()),
            ZERO_DIGEST,
            Source::Entry,
            None,
            NOW
        ),
        Ok(Accepted::Record {
            record,
            digest: ZERO_DIGEST
        })
    );
}

#[test]
fn a_bad_record_drops_alone_where_a_bad_name_drops_the_entry() {
    let me = identity(1);
    let peer = entry(&me, "m8.local", Some(&plain(&me)));
    let line = peer.line();
    let without = entry(&me, "m8.local", None);
    let word = |key: &str| {
        line.split(' ')
            .find(|word| word.starts_with(&format!("{key}=")))
            .expect("the word is there")
            .to_owned()
    };
    let (mname, mname_sig) = (word("mname"), word("mname_sig"));
    for bad in [
        line.replace(&mname, "mname=!!!!"),
        line.replace(&mname, &format!("{mname}=")),
        line.replace(&mname_sig, "mname_sig=00"),
        line.replace(&format!(" {mname_sig}"), ""),
        line.replace(&format!(" {mname}"), ""),
    ] {
        let parsed = crate::fleet_member::Peer::parse(&bad).expect("the entry stands");
        assert_eq!(parsed, without, "{bad}");
        assert!(parsed.signature_holds());
    }
    // Negative control: a malformed signed name drops the whole entry, as it always has.
    let name_sig = word("name_sig");
    assert_eq!(
        crate::fleet_member::Peer::parse(&line.replace(&name_sig, "name_sig=00")),
        None
    );
    // A record on an unsigned entry is no record.
    let mut unsigned = without.clone();
    unsigned.signed = None;
    let bare = format!("{} {mname} {mname_sig}", unsigned.line());
    let parsed = crate::fleet_member::Peer::parse(&bare).expect("the entry stands");
    assert_eq!(parsed.signed, None);
}

#[test]
fn a_registration_carries_no_record() {
    let me = identity(1);
    let signed = entry(&me, "m8.local", Some(&plain(&me))).signed;
    assert!(signed.is_some_and(|s| s.record().is_some()), "control");
    let registration = crate::fleet_member::Registration {
        node: me.node().to_owned(),
        host: "m8.local".to_owned(),
        port: 7485,
        version: "0.160.0".to_owned(),
        key: Some(me.key()),
        signed,
    };
    let line = registration.line();
    assert!(!line.contains("mname"), "{line}");
    let back = crate::fleet_member::Registration::parse(line.trim_end()).expect("the line parses");
    assert_eq!(back.signed.and_then(|s| s.record().copied()), None);
}

#[test]
fn the_widest_record_fits_the_longest_book_line() {
    // m8's longest book line, 529 bytes (§3.2), with the widest record on it.
    let me = identity(1);
    let base = entry(&me, "h", None).line().len();
    let host = "h".repeat(1 + 529 - base);
    let without = entry(&me, &host, None).line();
    assert_eq!(without.len(), 529);
    let with = entry(&me, &host, Some(&widest(&me))).line();
    assert_eq!(with.len(), 529 + WORDS_MAX);
    assert!(
        with.len() <= crate::fleet_member::RECORD_MAX,
        "{}",
        with.len()
    );
    assert!(crate::fleet_member::Peer::parse(&with).is_some());
}

#[test]
fn a_record_with_more_earlier_names_than_the_layout_carries_is_not_signed() {
    let me = identity(1);
    let mut record = widest(&me);
    assert!(
        record.sign(&me, ZERO_DIGEST).is_some(),
        "control: eight sign"
    );
    record.claim.was.push(Was {
        num: num(5),
        retired: false,
        lost: false,
    });
    assert_eq!(record.sign(&me, ZERO_DIGEST), None);
}

#[test]
fn a_record_at_rev_0_is_not_signed() {
    let me = identity(1);
    let mut record = plain(&me);
    assert!(record.sign(&me, ZERO_DIGEST).is_some(), "control");
    record.rev = 0;
    assert_eq!(record.sign(&me, ZERO_DIGEST), None);
}

#[test]
fn a_name_filled_in_at_one_seq_keeps_the_record_the_book_holds() {
    let me = identity(1);
    let unnamed = |record: Option<&NameRecord>| {
        let mut peer = crate::fleet_member::Peer::own(&me, ("m8.local", 7485, "0.160.0"), NOW);
        peer.signed = peer
            .signed
            .map(|signed| signed.with_record(record.and_then(|r| r.sign(&me, ZERO_DIGEST))));
        peer
    };
    let held = unnamed(Some(&plain(&me)));
    let named = unnamed(None).with_primary_name(&me, Some("m8"));
    assert!(
        named.signed.is_some_and(|s| s.record().is_none()),
        "control"
    );
    let mut book = vec![held.clone()];
    crate::fleet_member::record(&mut book, named).expect("records");
    let kept = book[0].signed.expect("signed");
    assert_eq!(book[0].primary_name(), Some("m8"), "the name filled in");
    assert_eq!(
        kept.record(),
        held.signed.as_ref().and_then(|s| s.record()),
        "the record stays"
    );
}

/// A private state directory of its own under the temp dir, as a node's state is.
fn private_state(tag: &str) -> std::path::PathBuf {
    use std::os::unix::fs::DirBuilderExt as _;
    let root = std::env::temp_dir().join(format!(
        "aterm-name-record-{tag}-{}-{}",
        std::process::id(),
        aterm_uds::rand::hex_token::<4>().expect("a token")
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .expect("a private dir");
    root
}

fn book(dir: &std::path::Path, file: &str, peers: &[crate::fleet_member::Peer]) {
    let text: String = peers.iter().map(|peer| peer.line() + "\n").collect();
    aterm_uds::private_file::write_private(dir, file, text.as_bytes()).expect("writes the book");
}

#[test]
fn the_nicknames_in_view_are_other_nodes_records_their_own_keys_sign() {
    use crate::fleet_member::{ISSUER_BOOK_FILE, PEERS_FILE};
    let dir = private_state("nicks");
    let (me, other, third) = (identity(1), identity(2), identity(3));
    let nick = |word: &str| Nickname::parse(word).expect("a nickname");
    let named = |who: &NodeIdentity, word: &str| {
        let mut record = plain(who);
        record.nick = Some(nick(word));
        record
    };
    assert!(
        nicknames_in_books(&dir, Some(me.node()), ZERO_DIGEST).is_empty(),
        "no books"
    );

    let fleet = [7; 8];
    let mut seeded = entry(&third, "m10.local", None);
    seeded.signed = seeded
        .signed
        .map(|signed| signed.with_record(named(&third, "otter").sign(&third, fleet)));
    // Another key's signature on `other`'s entry: no record of `other`'s.
    let mut forged = entry(&other, "m9.local", None);
    forged.signed = forged
        .signed
        .map(|signed| signed.with_record(named(&other, "puffin").sign(&third, ZERO_DIGEST)));
    book(
        &dir,
        PEERS_FILE,
        &[
            entry(&me, "m8.local", Some(&named(&me, "kestrel"))),
            entry(&other, "m9.local", Some(&named(&other, "heron"))),
            forged,
        ],
    );
    book(&dir, ISSUER_BOOK_FILE, &[seeded]);
    assert_eq!(
        nicknames_in_books(&dir, Some(me.node()), ZERO_DIGEST),
        [nick("heron")],
        "never this node's own, another fleet's or a record another key signed"
    );
    assert_eq!(
        nicknames_in_books(&dir, Some(me.node()), fleet),
        [nick("heron"), nick("otter")],
        "the held seed's records, and the zero digest's beside them"
    );
    assert_eq!(
        nicknames_in_books(&dir, None, ZERO_DIGEST),
        [nick("kestrel"), nick("heron")],
        "a node with no identity yet reads every one"
    );
    std::fs::remove_dir_all(&dir).expect("cleans up");
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::stores::Generation;
use aterm_db::table::store::{AppendKind, Appended, Via};

/// A store append feeds the egress its record with its provenance: a local append (a write,
/// or what an old peer's repair round brought) is published, one the mesh brought is not.
#[test]
fn an_append_feeds_its_record_marked_by_how_it_came() {
    for (via, mesh_borne) in [(Via::Local, false), (Via::Mesh, true)] {
        let appended = Appended {
            table: [7; 32],
            recovery: true,
            kind: AppendKind::Conflict,
            offset: 4,
            length: 5,
            via,
            wire: b"wire",
        };
        let Feed::Table {
            key,
            length,
            record: Some((held, borne)),
        } = table_feed(&appended)
        else {
            panic!("a table feed with its record");
        };
        assert_eq!(
            key,
            TableKey {
                table: [7; 32],
                recovery: true
            }
        );
        assert_eq!((length, held.offset, held.kind), (5, 4, Kind::Conflict));
        assert_eq!(held.wire, b"wire");
        assert_eq!(borne, mesh_borne);
    }
}

/// The vault's stores read the membership log's rewrite generation off its records entry's pin
/// (`aterm_secrets::client::Paths::records_generation`): 0 with no rewrite named where the
/// entry was never rewritten (no pin, as on every Mac of this build), a rewrite's pin's number
/// with the header it names, so another rewrite of the same number is another generation, and
/// an error, never 0, for a pin that begins as a rewrite's and is not one, so the egress's
/// members cursor is not read under a generation the entry is not at
/// ([`crate::egress::MembersCursor`]).
///
/// NEGATIVE CONTROL: [`Stores::members_generation`] left at its default (no generation read,
/// as before FL-148D), seen to fail with ``assertion `left == right` failed``, `left:
/// Generation { number: 0, rewrite: None }`, `right: Generation { number: 3, rewrite:
/// Some([171, 171, …]) }`.
#[test]
fn the_members_generation_is_the_records_entrys() {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};

    let home = aterm_tempfile::tempdir().expect("a home");
    let paths = Paths::locate_for_home(home.path());
    let stores = VaultStores::new(paths.clone(), home.path().join("aterm.toml"));
    assert_eq!(
        stores.members_generation().expect("no pin"),
        Generation::default()
    );

    let entry = paths.records_file();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(entry.parent().expect("the vault's folder"))
        .expect("the vault's folder");
    let pin = entry.with_file_name("records.layout");
    let write = |text: &str| {
        let _ = std::fs::remove_file(&pin);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&pin)
            .expect("the pin");
        io::Write::write_all(&mut file, text.as_bytes()).expect("the pin's text");
    };
    write(&format!(
        "aterm-records-pin/2\n{}\ncapacity 67108864\ngeneration 3\n",
        "ab".repeat(32)
    ));
    assert_eq!(
        stores.members_generation().expect("a rewrite's pin"),
        Generation {
            number: 3,
            rewrite: Some([0xab; 32]),
        }
    );
    // Another rewrite of the same number is another generation.
    write(&format!(
        "aterm-records-pin/2\n{}\ncapacity 67108864\ngeneration 3\n",
        "cd".repeat(32)
    ));
    assert_eq!(
        stores.members_generation().expect("a rewrite's pin"),
        Generation {
            number: 3,
            rewrite: Some([0xcd; 32]),
        }
    );
    write("aterm-records-pin/2\nnot a rewrite's pin\n");
    assert!(stores.members_generation().is_err());
}

/// A version-2 membership record's frame as `aterm_secrets::membership_log` lays one out
/// (no parents, every field zero but its lengths and the SHA-256 of `ciphertext` its clear
/// fields end with), whole or with the ciphertext shed. It is signed by no key: an id is read
/// off the bytes alone.
fn version_two(ciphertext: &[u8], shed: bool) -> Vec<u8> {
    let mut wire = b"ALM1".to_vec();
    wire.push(2);
    // Network, kind, actor, sequence, previous, no parents, then the authored time.
    wire.extend_from_slice(&[0; 32 + 1 + 48 + 8 + 32 + 1 + 8]);
    wire.extend_from_slice(&36u32.to_be_bytes());
    let tail = if shed { &[][..] } else { ciphertext };
    wire.extend_from_slice(&u32::try_from(tail.len()).expect("a length").to_be_bytes());
    wire.extend_from_slice(
        &u32::try_from(ciphertext.len())
            .expect("a length")
            .to_be_bytes(),
    );
    wire.extend_from_slice(&crate::stores::digest(ciphertext));
    wire.extend_from_slice(&[0; 64]);
    wire.extend_from_slice(tail);
    wire
}

/// The vault's stores know a membership record by its own id ([`Stores::member_id`]), read
/// off its bytes (`aterm_secrets::membership_log::record_id`): a version-2 record's is one id
/// whole and with its ciphertext shed, the digest of its shed frame, so the egress finds the
/// last record it stepped past in a log whose rewrite shed that record's body
/// ([`crate::egress::MembersCursor`]). Bytes that frame no record are known by their digest.
///
/// NEGATIVE CONTROL: [`Stores::member_id`] left at its default (the digest of the bytes, as
/// before the slice B review), seen to fail with ``assertion `left == right` failed`` on the
/// whole record's id and the shed one's.
#[test]
fn a_record_is_known_by_its_id_whole_and_shed() {
    let home = aterm_tempfile::tempdir().expect("a home");
    let stores = VaultStores::new(
        Paths::locate_for_home(home.path()),
        home.path().join("aterm.toml"),
    );
    let (whole, shed) = (version_two(b"a body", false), version_two(b"a body", true));
    assert_eq!(whole.len(), shed.len() + b"a body".len());
    assert_eq!(stores.member_id(&whole), stores.member_id(&shed));
    assert_eq!(stores.member_id(&shed), crate::stores::digest(&shed));
    assert_ne!(stores.member_id(&whole), crate::stores::digest(&whole));
    // Not the ciphertext its clear fields name: no record, so its bytes' digest.
    let mut other = whole;
    *other.last_mut().expect("a ciphertext") ^= 1;
    assert_eq!(stores.member_id(&other), crate::stores::digest(&other));
    assert_eq!(
        stores.member_id(b"no record"),
        crate::stores::digest(b"no record")
    );
}

/// A vault's genesis, founded by one Mac, and a record that Mac signed after it.
fn founded() -> (Record, Record) {
    use aterm_secrets::commons::binding::Request;
    use aterm_secrets::membership_log::{Body, Entrant, Log};

    let request = Request::create(
        [2; 16],
        [3; 32],
        [1; 16],
        [1; 16],
        &aterm_secrets::age::Identity::from_secret_bytes([0x54; 32]),
        &[1; 32],
    )
    .expect("a device request");
    let member = request.binding().actor();
    let genesis = Record::found(
        &[99; 32],
        vec![Entrant {
            request,
            node_key: [7; 32],
            name: "mac-1".to_owned(),
            label: "m1".to_owned(),
        }],
        1,
    )
    .expect("a genesis");
    let log = Log::new(genesis.clone()).expect("a log");
    let named = log
        .draft(
            &member,
            2,
            Body::Name {
                name: "mac-one".to_owned(),
            },
        )
        .sign(&[1; 32], [1; 16])
        .expect("a signed record");
    (genesis, named)
}

/// A WRITE'S HOOK HANDS THE EGRESS THE GENERATION THE WRITE SAYS IT WROTE UNDER (tracker
/// FL-148E, carried from FL-148D). The hook `install` installs feeds the open egress
/// [`written_feed`] of each durable write of the records file: the records from the egress's
/// cursor on, under the write's own generation
/// (`aterm_secrets::membership_log::Written::generation`). Here a real write of a vault's
/// records file (the lane's ingest) runs that function in the hook's place: what it hands is
/// under the generation the vault's stores read off the records entry
/// ([`Stores::members_generation`], which the egress's cursor counts under), so the egress
/// takes the records and does not read the file again for them; and a write that says a
/// rewrite's generation is handed on under that one, number and rewrite both.
///
/// NEGATIVE CONTROLS, each made in `written_feed` and seen to fail: the generation moved
/// (`number + 7` under a rewrite, the slice B review's mutation, which every other test of
/// this crate passed) with ``assertion `left == right` failed: the genesis write's feed``,
/// `left: … generation: Generation { number: 7, rewrite: Some([9, 9, …]) } …`; and the
/// generation of an entry never rewritten handed whatever the write says, with ``assertion
/// `left == right` failed: a rewritten entry's write``, `left: … Generation { number: 0,
/// rewrite: None } …`.
#[test]
fn a_writes_hook_hands_the_egress_the_generation_it_wrote_under() {
    use aterm_secrets::membership_log::Written;
    use aterm_secrets::membership_log::store::with_written;
    use std::cell::RefCell;
    use std::rc::Rc;

    let home = aterm_tempfile::tempdir().expect("a home");
    let stores = VaultStores::new(
        Paths::locate_for_home(home.path()),
        home.path().join("aterm.toml"),
    );
    let (genesis, named) = founded();
    let wire = |record: &Record| record.wire_bytes().expect("its bytes").into_owned();
    let rewritten = aterm_secrets::client::RecordsGeneration {
        number: 3,
        rewrite: Some([0xab; 32]),
    };
    // What the hook feeds an egress whose cursor is at `from` for the write that takes
    // `record`: as the write says, and as a write of a rewritten entry would say.
    let fed = |from: usize, record: &Record| {
        let feeds: Rc<RefCell<Vec<Feed>>> = Rc::default();
        let seen = feeds.clone();
        let took = with_written(
            move |written| {
                let said = Written {
                    paths_dir: written.paths_dir,
                    store: written.store,
                    generation: rewritten,
                };
                seen.borrow_mut()
                    .extend([written_feed(written, from), written_feed(&said, from)]);
            },
            || stores.ingest_members(&[wire(record)]),
        );
        assert_eq!(took, vec![Verdict::Applied]);
        let feeds = feeds.borrow().clone();
        <[Feed; 2]>::try_from(feeds).expect("one write, read twice")
    };
    let handed =
        |length: usize, from: usize, generation: Generation, record: &Record| Feed::Members {
            length,
            records: Some(crate::egress::Handed {
                from,
                generation,
                records: vec![wire(record)],
            }),
        };
    let of_a_rewrite = Generation {
        number: 3,
        rewrite: Some([0xab; 32]),
    };

    let [written, said] = fed(0, &genesis);
    let entry = stores.members_generation().expect("the entry's generation");
    assert_eq!(
        written,
        handed(1, 0, entry, &genesis),
        "the genesis write's feed"
    );
    assert_eq!(
        said,
        handed(1, 0, of_a_rewrite, &genesis),
        "a rewritten entry's write"
    );
    // The next write, read by an egress that stepped past the genesis.
    let [written, said] = fed(1, &named);
    assert_eq!(written, handed(2, 1, entry, &named));
    assert_eq!(said, handed(2, 1, of_a_rewrite, &named));
}

/// A Mac whose records are past format 1 neither feeds nor takes the mesh's members lane
/// (ATERM-KI-1116-1471, `aterm_secrets::client::mesh_members`): the lane states no records
/// bound, so no `409` fences it. With no format mark the vault's stores take a membership
/// record the lane brings, and hold, offer and send it. Once the Mac's format mark names
/// format 2 (written here as the file a follow stores, `records.format` beside the records;
/// no running build stores one) they read the lane an empty log, whose inventory, held records
/// and wires are none, and a record the lane brings is refused for good.
///
/// They say the lane is shut too ([`Stores::members_lane`]), so a repair round leaves the
/// membership unit out and a format-1 neighbour offers this Mac nothing
/// (`crate::inventory`'s `a_mac_past_format_one_leaves_the_membership_unit_out_of_a_repair_round`).
///
/// NEGATIVE CONTROL: `VaultStores::members` reading `aterm_secrets::client::load_records` (as
/// before the fence), seen to fail with ``a marked Mac offers the lane nothing``; and with
/// `VaultStores` leaving `members_lane` at the trait's `true`, seen to fail with ``a marked
/// Mac's lane is shut``.
#[test]
fn a_mac_past_format_one_neither_feeds_nor_takes_the_members_lane() {
    use std::os::unix::fs::OpenOptionsExt as _;

    let home = aterm_tempfile::tempdir().expect("a home");
    let paths = Paths::locate_for_home(home.path());
    let stores = VaultStores::new(paths.clone(), home.path().join("aterm.toml"));
    let (genesis, named) = founded();
    let wire = |record: &Record| record.wire_bytes().expect("its bytes").into_owned();

    assert_eq!(
        stores.ingest_members(&[wire(&genesis)]),
        vec![Verdict::Applied]
    );
    assert_eq!(
        stores.members_inventory().expect("the inventory"),
        BTreeSet::from([*genesis.id()])
    );
    assert_eq!(
        stores.members_held(0).expect("the held records"),
        (vec![wire(&genesis)], 1)
    );
    assert!(stores.members_lane().expect("the lane"));

    let mark = paths.records_file().with_file_name("records.format");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&mark)
        .expect("the format mark");
    io::Write::write_all(&mut file, b"aterm-secrets records format 2\n").expect("its text");
    drop(file);
    assert!(
        !stores.members_lane().expect("the lane"),
        "a marked Mac's lane is shut"
    );
    assert!(
        stores
            .members_inventory()
            .expect("the inventory")
            .is_empty(),
        "a marked Mac offers the lane nothing"
    );
    assert_eq!(
        stores.members_held(0).expect("the held records"),
        (Vec::new(), 0)
    );
    assert!(
        stores
            .members_wires(&[*genesis.id()])
            .expect("the wires")
            .is_empty()
    );
    let took = stores.ingest_members(&[wire(&named)]);
    assert!(
        matches!(took.as_slice(), [Verdict::Refused(_)]),
        "a marked Mac takes nothing the lane brings: {took:?}"
    );
}

/// A Mac past format 1 states no membership digest in its presence row, so no repair round is
/// asked for its membership store, on its side or a neighbour's (ATERM-KI-1116-1471, tracker
/// FL-148E3). With no format mark its row states the digest of the records its vault holds
/// ([`crate::presence::membership_of_vault`]), and the digest trigger asks a round of a
/// neighbour whose digest differs two rows in a row. Once its format mark names format 2 (the
/// file a follow stores, written here; no running build stores one) its records ride no members
/// lane and its row states none: the trigger asks nothing of a neighbour whatever that
/// neighbour holds, and a neighbour that reads its row asks nothing of it either, since the
/// trigger asks only where both digests are known. A round would move no membership record and
/// reconcile every table both hold for nothing; a round its tables ask still runs.
///
/// NEGATIVE CONTROL: `membership_of_vault` stating the store's digest whatever the lane says
/// (as before), run against this test and seen to fail with ``a marked Mac states no
/// membership digest``.
#[test]
fn a_mac_past_format_one_states_no_membership_digest_so_no_round_is_asked_for_it() {
    use crate::presence::{Held, membership_of_vault};
    use aterm_mesh_core::relay_digest::Trigger;
    use std::os::unix::fs::OpenOptionsExt as _;

    let home = aterm_tempfile::tempdir().expect("a home");
    let paths = Paths::locate_for_home(home.path());
    let stores = VaultStores::new(paths.clone(), home.path().join("aterm.toml"));
    let (genesis, _) = founded();
    let wire = genesis.wire_bytes().expect("its bytes").into_owned();
    assert_eq!(stores.ingest_members(&[wire]), vec![Verdict::Applied]);

    let theirs = Some(Held::membership(&BTreeSet::from([[9; 32]])));
    // Two rows of a neighbour, read against this Mac's own digest `mine`: whether each asks.
    let rows = |theirs: Option<Held>, mine: Option<Held>| {
        let mut trigger = Trigger::default();
        [
            trigger.row(theirs, mine, true, false),
            trigger.row(theirs, mine, true, false),
        ]
    };
    let unmarked = membership_of_vault(&paths);
    assert_eq!(
        unmarked,
        Some(Held::membership(&BTreeSet::from([*genesis.id()])))
    );
    assert_eq!(
        rows(theirs, unmarked),
        [false, true],
        "a digest that differs asks"
    );

    let mark = paths.records_file().with_file_name("records.format");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&mark)
        .expect("the format mark");
    io::Write::write_all(&mut file, b"aterm-secrets records format 2\n").expect("its text");
    drop(file);
    let marked = membership_of_vault(&paths);
    assert_eq!(marked, None, "a marked Mac states no membership digest");
    assert_eq!(rows(theirs, marked), [false, false], "it asks no round");
    assert_eq!(
        rows(marked, unmarked),
        [false, false],
        "a neighbour reading its row asks none of it"
    );
}

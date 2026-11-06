// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tables over the relay (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 B7b): in-process Macs (the
//! B7a lab of [`super`]) whose meshes move the records of stand-in stores. Each stand-in keeps its
//! tables and its membership log in files, so a second process can append to them as a CLI's write
//! does; it judges a table operation as the Commons store does in the one way these tests need: an
//! operation whose writer holds no seat in its membership log waits. Every wait here is the
//! one-minute hang detector.

use super::{HANG, Lab, Mac, lock, seating};
use crate::egress::{Egress, Feed, Wake};
use crate::inventory::Reason;
use crate::stores::{self, Digest, Held, Kind, ProofKey, Stores, TableKey, Verdict};
use crate::{Event, MeshLog};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Instant;

/// The table every test writes.
const TABLE: Digest = [0x5a; 32];

/// The child process's mode: append one record to a stand-in's table or membership log and wake
/// the egress.
const CHILD_DIR: &str = "ATERM_TEST_MESH_STORE_DIR";
const CHILD_STATE: &str = "ATERM_TEST_MESH_STATE_DIR";
const CHILD_WHAT: &str = "ATERM_TEST_MESH_APPEND";

/// A stand-in store: one file per table (`t-<hex>`, a line per record: its kind's byte and its
/// wire in hex) and one for the membership log (`members`, a line per record in hex). A membership
/// record `seat:<writer>` seats a writer; a table operation is `<writer>|<text>`.
struct Fake {
    dir: PathBuf,
    egress: Mutex<Option<Weak<Egress>>>,
    changed: Mutex<u64>,
    moved: Condvar,
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn append_line(path: &Path, line: &str) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("a stand-in store file");
    writeln!(file, "{line}").expect("a stand-in record");
}

fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}

fn unhex(text: &str) -> Vec<u8> {
    let mut out = vec![0u8; text.len() / 2];
    assert!(aterm_codec::hex::decode_into(text.as_bytes(), &mut out));
    out
}

/// A stand-in conflict proof's key: its wire's digest after the 48 bytes of actor and slot, with
/// no fork flag.
fn proof_key(wire: &[u8]) -> ProofKey {
    let mut key = [0u8; 81];
    key[49..].copy_from_slice(&stores::digest(wire));
    key
}

impl Fake {
    fn new(dir: &Path) -> Arc<Self> {
        std::fs::create_dir_all(dir).expect("a stand-in store dir");
        Arc::new(Self {
            dir: dir.to_path_buf(),
            egress: Mutex::new(None),
            changed: Mutex::new(0),
            moved: Condvar::new(),
        })
    }

    fn table_file(&self, table: &Digest) -> PathBuf {
        self.dir.join(format!("t-{}", hex(table)))
    }

    fn members_file(&self) -> PathBuf {
        self.dir.join("members")
    }

    /// The egress this store's appends feed, as a process's sink does.
    fn serve(&self, egress: &Arc<Egress>) {
        *lock(&self.egress) = Some(Arc::downgrade(egress));
    }

    fn egress(&self) -> Option<Arc<Egress>> {
        lock(&self.egress).as_ref().and_then(Weak::upgrade)
    }

    fn records(&self, table: &Digest) -> Vec<(Kind, Vec<u8>)> {
        lines(&self.table_file(table))
            .iter()
            .filter_map(|line| {
                let (kind, wire) = line.split_once(' ')?;
                Some((Kind::of(kind.parse().ok()?)?, unhex(wire)))
            })
            .collect()
    }

    fn members(&self) -> Vec<Vec<u8>> {
        lines(&self.members_file())
            .iter()
            .map(|line| unhex(line))
            .collect()
    }

    fn seated(&self, writer: &str) -> bool {
        self.members()
            .iter()
            .any(|record| record.as_slice() == format!("seat:{writer}").as_bytes())
    }

    fn holds(&self, text: &str) -> bool {
        self.records(&TABLE)
            .iter()
            .any(|(_, wire)| wire.ends_with(format!("|{text}").as_bytes()))
    }

    fn holds_kind(&self, kind: Kind, text: &str) -> bool {
        self.records(&TABLE)
            .iter()
            .any(|(k, wire)| *k == kind && wire.ends_with(format!("|{text}").as_bytes()))
    }

    fn bump(&self) {
        *lock(&self.changed) += 1;
        self.moved.notify_all();
    }

    /// Wait until this store holds the operation whose text is `text`.
    fn until_holds(&self, text: &str) {
        let deadline = Instant::now() + HANG;
        let mut changed = lock(&self.changed);
        while !self.holds(text) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "{} did not come to hold {text:?} within the hang detector",
                self.dir.display()
            );
            changed = self
                .moved
                .wait_timeout(changed, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// Append `wire` of `kind` to `table`, feeding the egress as the store's sink does.
    fn append(&self, table: &Digest, kind: Kind, wire: &[u8], mesh: bool) {
        let file = self.table_file(table);
        append_line(&file, &format!("{} {}", kind.byte(), hex(wire)));
        let length = lines(&file).len() as u64;
        if let Some(egress) = self.egress() {
            egress.feed(Feed::Table {
                key: TableKey {
                    table: *table,
                    recovery: false,
                },
                length,
                record: Some((
                    Held {
                        offset: length - 1,
                        kind,
                        wire: wire.to_vec(),
                    },
                    mesh,
                )),
            });
        }
        self.bump();
    }

    /// Write `record` to the membership log, feeding the egress as its records file's hook does.
    fn append_member(&self, record: &[u8]) {
        append_line(&self.members_file(), &hex(record));
        if let Some(egress) = self.egress() {
            let all = self.members();
            let from = egress.members_cursor().unwrap_or(0);
            egress.feed(Feed::Members {
                length: all.len(),
                records: Some((from, all.get(from..).unwrap_or_default().to_vec())),
            });
        }
        self.bump();
    }

    /// A local write: `writer` writes `text` to the test's table.
    fn write(&self, writer: &str, text: &str) {
        self.append(
            &TABLE,
            Kind::Operation,
            format!("{writer}|{text}").as_bytes(),
            false,
        );
    }

    /// Seat `writer` in this store's membership log, with no egress fed: history before the mesh.
    fn seed_seat(&self, writer: &str) {
        append_line(
            &self.members_file(),
            &hex(format!("seat:{writer}").as_bytes()),
        );
    }
}

impl Stores for Fake {
    fn ingest_table(&self, table: Digest, kind: Kind, wires: &[Vec<u8>]) -> Vec<Verdict> {
        wires
            .iter()
            .map(|wire| {
                let text = String::from_utf8_lossy(wire).into_owned();
                let Some((writer, _)) = text.split_once('|') else {
                    return Verdict::Refused("no writer".to_owned());
                };
                if self
                    .records(&table)
                    .iter()
                    .any(|(k, held)| *k == kind && held == wire)
                {
                    return Verdict::Applied;
                }
                if !self.seated(writer) {
                    return Verdict::Waiting(format!("{writer} holds no seat"));
                }
                self.append(&table, kind, wire, true);
                Verdict::Applied
            })
            .collect()
    }

    fn ingest_members(&self, records: &[Vec<u8>]) -> Vec<Verdict> {
        records
            .iter()
            .map(|record| {
                if !self.members().contains(record) {
                    self.append_member(record);
                }
                Verdict::Applied
            })
            .collect()
    }

    fn table_lengths(&self) -> io::Result<Vec<(TableKey, u64)>> {
        let mut lengths = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if let Some(text) = name.strip_prefix("t-") {
                let mut table = [0u8; 32];
                assert!(aterm_codec::hex::decode_into(text.as_bytes(), &mut table));
                lengths.push((
                    TableKey {
                        table,
                        recovery: false,
                    },
                    self.records(&table).len() as u64,
                ));
            }
        }
        Ok(lengths)
    }

    fn table_held(&self, key: TableKey, from: u64, upto: u64) -> io::Result<Vec<Held>> {
        Ok(self
            .records(&key.table)
            .into_iter()
            .enumerate()
            .map(|(offset, (kind, wire))| Held {
                offset: offset as u64,
                kind,
                wire,
            })
            .filter(|held| held.offset >= from && held.offset < upto)
            .collect())
    }

    fn members_held(&self, from: usize) -> io::Result<(Vec<Vec<u8>>, usize)> {
        let all = self.members();
        Ok((all.get(from..).unwrap_or_default().to_vec(), all.len()))
    }

    fn tables(&self) -> io::Result<BTreeSet<Digest>> {
        Ok(self
            .table_lengths()?
            .into_iter()
            .map(|(key, _)| key.table)
            .collect())
    }

    fn table_inventory(&self, table: Digest) -> io::Result<Option<BTreeSet<Digest>>> {
        let records = self.records(&table);
        Ok(
            (!records.is_empty() || self.table_file(&table).exists()).then(|| {
                records
                    .iter()
                    .filter(|(kind, _)| *kind == Kind::Operation)
                    .map(|(_, wire)| stores::digest(wire))
                    .collect()
            }),
        )
    }

    fn table_wires(&self, table: Digest, ids: &[Digest]) -> io::Result<Vec<Vec<u8>>> {
        let wanted: BTreeSet<Digest> = ids.iter().copied().collect();
        Ok(self
            .records(&table)
            .into_iter()
            .filter(|(_, wire)| wanted.contains(&stores::digest(wire)))
            .map(|(_, wire)| wire)
            .collect())
    }

    fn table_proofs(&self, table: Digest) -> io::Result<Option<BTreeSet<ProofKey>>> {
        let records = self.records(&table);
        Ok(
            (!records.is_empty() || self.table_file(&table).exists()).then(|| {
                records
                    .iter()
                    .filter(|(kind, _)| *kind == Kind::Conflict)
                    .map(|(_, wire)| proof_key(wire))
                    .collect()
            }),
        )
    }

    fn table_proof_wires(&self, table: Digest, keys: &[ProofKey]) -> io::Result<Vec<Vec<u8>>> {
        let wanted: BTreeSet<ProofKey> = keys.iter().copied().collect();
        Ok(self
            .records(&table)
            .into_iter()
            .filter(|(kind, wire)| *kind == Kind::Conflict && wanted.contains(&proof_key(wire)))
            .map(|(_, wire)| wire)
            .collect())
    }

    fn members_inventory(&self) -> io::Result<BTreeSet<Digest>> {
        Ok(self.members().iter().map(|r| stores::digest(r)).collect())
    }

    fn members_wires(&self, ids: &[Digest]) -> io::Result<Vec<Vec<u8>>> {
        let wanted: BTreeSet<Digest> = ids.iter().copied().collect();
        Ok(self
            .members()
            .into_iter()
            .filter(|r| wanted.contains(&stores::digest(r)))
            .collect())
    }
}

/// A Mac whose mesh moves the records of a stand-in store in its state dir, every writer of
/// `seats` seated in it before the mesh starts.
fn tables_mac(lab: &Lab, tag: u8, seats: &[&str]) -> (Mac, Arc<Fake>) {
    let mut mac = Mac::new(lab, tag);
    let fake = Fake::new(&mac.dir.path().join("stores"));
    for writer in seats {
        fake.seed_seat(writer);
    }
    mac.stores = Some(Arc::clone(&fake) as Arc<dyn Stores>);
    (mac, fake)
}

/// Start each Mac, seated in all, its store serving its egress, and wait until every relay follows
/// and the first `Inventory` of each new link ended.
fn start_all(lab: &Lab, macs: &mut [(Mac, Arc<Fake>)]) {
    let all = seating(&macs.iter().map(|(mac, _)| mac).collect::<Vec<_>>());
    for (mac, fake) in macs.iter_mut() {
        mac.read(all.clone());
        mac.start(lab);
        fake.serve(&mac.mesh().egress().expect("the mesh moves tables"));
    }
    for (mac, _) in macs.iter() {
        let peers: Vec<&Mac> = macs
            .iter()
            .map(|(m, _)| m)
            .filter(|m| m.key != mac.key)
            .collect();
        mac.until_following(&peers, 0);
    }
    for (mac, _) in macs.iter() {
        let others = macs.len() - 1;
        mac.seen
            .until("the first Inventory of each link", |events| {
                started(events, 0) + skipped(events) >= others
            });
    }
}

/// The `Inventory` exchanges a Mac started from event `since` on.
fn started(events: &[Event], since: usize) -> usize {
    events
        .iter()
        .skip(since)
        .filter(|event| {
            matches!(
                event,
                Event::Inventory {
                    reason: Some(_),
                    ..
                }
            )
        })
        .count()
}

/// The `Inventory` exchanges a Mac was asked for and did not run.
fn skipped(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, Event::InventorySkipped { .. }))
        .count()
}

/// The records a Mac put on `tables.log` under its own owner segment.
fn own(mac: &Mac) -> Vec<(String, String)> {
    mac.held(MeshLog::Tables, &format!("/t/*/{}", mac.principal))
}

/// A write on one of three Macs projects on the other two by the relay alone: no `Inventory` runs
/// after the write, and each Mac that ingests it publishes nothing of its own. With the egress made
/// to publish what the mesh delivered (the negative control), the receivers republish it.
#[test]
fn a_write_projects_on_the_other_two_with_no_repair_round() {
    let lab = Lab::new();
    let mut macs: Vec<(Mac, Arc<Fake>)> =
        (1..=3).map(|tag| tables_mac(&lab, tag, &["w"])).collect();
    start_all(&lab, &mut macs);
    let marks: Vec<usize> = macs.iter().map(|(mac, _)| mac.seen.mark()).collect();
    macs[0].1.write("w", "first");
    macs[1].1.until_holds("first");
    macs[2].1.until_holds("first");
    for ((mac, _), mark) in macs.iter().zip(&marks) {
        let events = lock(&mac.seen.events).clone();
        assert_eq!(
            started(&events, *mark),
            0,
            "{} ran an Inventory",
            mac.principal
        );
    }
    assert_eq!(
        own(&macs[0].0).len(),
        1,
        "the writer published its write once"
    );
    for (mac, _) in &macs[1..] {
        // Its egress has settled the delivered record, so the check below is not vacuous and the
        // negative control after it finds nothing of "first" still queued.
        let key = TableKey {
            table: TABLE,
            recovery: false,
        };
        assert!(mac.mesh().egress().expect("an egress").wait_published(
            key,
            1,
            Instant::now() + HANG
        ));
        assert!(
            own(mac).is_empty(),
            "{} republished what it ingested",
            mac.principal
        );
    }

    // The negative control: an egress that publishes what the mesh delivered republishes it.
    for (mac, _) in &macs[1..] {
        mac.mesh()
            .egress()
            .expect("an egress")
            .republish_for_test(true);
    }
    macs[0].1.write("w", "second");
    macs[1].1.until_holds("second");
    macs[2].1.until_holds("second");
    for (mac, _) in &macs[1..] {
        let egress = mac.mesh().egress().expect("an egress");
        let key = TableKey {
            table: TABLE,
            recovery: false,
        };
        assert!(egress.wait_published(key, 2, Instant::now() + HANG));
        assert_eq!(
            own(mac).len(),
            1,
            "{} republished the delivered record",
            mac.principal
        );
    }
}

/// Each Mac's tables digest once its egress has passed every record its store holds: the stores'
/// own inventory ([`crate::presence::Held::table`] of `table_inventory`), the same on every Mac.
fn settled_tables(macs: &[(Mac, Arc<Fake>)]) -> crate::presence::Held {
    let key = TableKey {
        table: TABLE,
        recovery: false,
    };
    let held: Vec<crate::presence::Held> = macs
        .iter()
        .map(|(mac, fake)| {
            let length = fake.records(&TABLE).len() as u64;
            let egress = mac.mesh().egress().expect("an egress");
            assert!(egress.wait_published(key, length, Instant::now() + HANG));
            let ids = fake
                .table_inventory(TABLE)
                .expect("the store reads")
                .expect("the store holds the table");
            let held = mac
                .mesh()
                .tables_held()
                .expect("the first catch-up read every store");
            assert_eq!(
                held,
                crate::presence::Held::tables(&BTreeMap::from([(
                    TABLE,
                    crate::presence::Held::table(&ids)
                )])),
                "{} reads what its store holds",
                mac.principal
            );
            held
        })
        .collect();
    assert!(held.windows(2).all(|pair| pair[0] == pair[1]), "{held:?}");
    held[0]
}

/// Macs that hold the same operations read the same tables digest in their presence rows (§10's
/// soak: "every instance's presence row shows the same tables digest"), and it is what their
/// stores hold: an operation from before the mesh counts (the first catch-up read it, no sink fed
/// it), as does one the mesh delivered; a write moves it on every Mac once it projects there; a
/// conflict record is no operation and moves nothing.
#[test]
fn macs_that_hold_the_same_operations_read_the_same_tables_digest() {
    let lab = Lab::new();
    let mut macs: Vec<(Mac, Arc<Fake>)> =
        (1..=3).map(|tag| tables_mac(&lab, tag, &["w"])).collect();
    for (_, fake) in &macs {
        fake.append(&TABLE, Kind::Operation, b"w|history", false);
    }
    start_all(&lab, &mut macs);
    let history = settled_tables(&macs);
    assert_eq!(history.count, 1, "the history the first catch-up read");

    macs[0].1.write("w", "first");
    for (_, fake) in &macs[1..] {
        fake.until_holds("first");
    }
    let written = settled_tables(&macs);
    assert_eq!(written.count, 2);
    assert_ne!(written.digest, history.digest, "a write moves the digest");

    macs[0]
        .1
        .append(&TABLE, Kind::Conflict, b"w|a-conflict", false);
    for (_, fake) in &macs[1..] {
        fake.until_holds("a-conflict");
        assert!(fake.holds_kind(Kind::Conflict, "a-conflict"));
    }
    assert_eq!(
        settled_tables(&macs),
        written,
        "a conflict record moves nothing"
    );
}

/// An operation an older peer delivered by an old path enters the store as a local append does,
/// and the egress puts it on the relay, so a Mac that speaks only `Bus` gets it.
#[test]
fn an_old_path_peers_operation_reaches_a_bus_only_mac_through_the_egress() {
    let lab = Lab::new();
    let mut macs: Vec<(Mac, Arc<Fake>)> =
        (1..=2).map(|tag| tables_mac(&lab, tag, &["old"])).collect();
    start_all(&lab, &mut macs);
    // What a repair round with an old peer appends on A is a local append (`Via::Local`:
    // aterm-db's `a_repair_round_appends_what_it_brings_as_local`), not a write of A's own
    // writer; B hears no repair round, only A's egress.
    macs[0]
        .1
        .append(&TABLE, Kind::Operation, b"old|from-an-old-peer", false);
    macs[1].1.until_holds("from-an-old-peer");
    assert_eq!(own(&macs[0].0).len(), 1);
    // The negative control: the same kind of record tagged as come by the mesh (`Via::Mesh`) is
    // not published, so B, which hears only the egress, never holds it.
    macs[0]
        .1
        .append(&TABLE, Kind::Operation, b"old|came-by-the-mesh", true);
    macs[0]
        .1
        .append(&TABLE, Kind::Operation, b"old|after", false);
    macs[1].1.until_holds("after");
    assert!(!macs[1].1.holds("came-by-the-mesh"));
    assert_eq!(own(&macs[0].0).len(), 2);
}

/// An operation whose writer's seat has not reached a Mac waits there, and applies once the seat's
/// membership record arrives. With waiting records never tried again (the negative control), it
/// stays out.
#[test]
fn an_operation_whose_writers_seat_has_not_arrived_waits_and_applies_after_it() {
    for retry in [true, false] {
        let lab = Lab::new();
        let mut macs: Vec<(Mac, Arc<Fake>)> =
            (1..=2).map(|tag| tables_mac(&lab, tag, &[])).collect();
        start_all(&lab, &mut macs);
        let (a, b) = (&macs[0], &macs[1]);
        let ingest = b.0.mesh().ingest().expect("an ingest consumer");
        ingest.no_retry_for_test(!retry);
        let mark = b.0.seen.mark();
        a.1.write("late", "waits");
        b.0.seen.until("the operation waits", |events| {
            events.iter().skip(mark).any(|event| {
                matches!(
                    event,
                    Event::Ingested {
                        members: false,
                        waiting: 1,
                        ..
                    }
                )
            })
        });
        assert!(!b.1.holds("waits"));
        assert_eq!(ingest.waiting(), 1);
        a.1.append_member(b"seat:late");
        b.0.seen.until("the seat arrives", |events| {
            events.iter().skip(mark).any(|event| {
                matches!(
                    event,
                    Event::Ingested {
                        members: true,
                        applied: 1,
                        ..
                    }
                )
            })
        });
        // A barrier: an operation of a writer B seats already lands in a later pass of B's worker
        // than the seat did, after that pass tried the waiting record again (or would have).
        a.1.write("late", "marker");
        b.1.until_holds("marker");
        if retry {
            b.1.until_holds("waits");
            b.0.seen.until("the retry reports", |events| {
                events.iter().skip(mark).any(|event| {
                    matches!(
                        event,
                        Event::Ingested {
                            members: false,
                            applied: 1,
                            waiting: 0,
                            ..
                        }
                    )
                })
            });
            assert_eq!(ingest.waiting(), 0);
        } else {
            assert!(
                !b.1.holds("waits"),
                "a waiting record applied with no retry"
            );
            assert_eq!(ingest.waiting(), 1);
        }
    }
}

/// A record cut from `tables.log` below a sleeping Mac's cursor reaches it by one `Inventory`,
/// which the relay's gap notice starts; with `NO_INVENTORY_ON_GAP` on, it never does.
#[test]
fn a_record_cut_below_a_sleeping_macs_cursor_reaches_it_by_one_inventory() {
    for gap_rule in [true, false] {
        crate::inventory::NO_INVENTORY_ON_GAP.with(|on| on.set(!gap_rule));
        let lab = Lab::new();
        let mut macs: Vec<(Mac, Arc<Fake>)> =
            (1..=2).map(|tag| tables_mac(&lab, tag, &["w"])).collect();
        start_all(&lab, &mut macs);
        macs[0].1.write("w", "before");
        macs[1].1.until_holds("before");
        // B sleeps; A writes, and its log is cut past what B's cursor holds.
        macs[1].0.stop();
        macs[0].1.write("w", "while-asleep");
        macs[0]
            .1
            .append(&TABLE, Kind::Conflict, b"w|a-conflict", false);
        let egress = macs[0].0.mesh().egress().expect("an egress");
        let key = TableKey {
            table: TABLE,
            recovery: false,
        };
        assert!(egress.wait_published(key, 3, Instant::now() + HANG));
        // A's own ingest group steps past A's records, and the cut stops at its floor, so it is B's
        // cursor alone the cut passes. (A group's commit is a record of the log too, so the
        // log's head runs past every record the group reads.)
        let last = egress
            .last_published(&TABLE)
            .expect("A published the table");
        let ingest = macs[0].0.mesh().ingest().expect("an ingest consumer");
        assert!(ingest.wait_committed_for_test(last, Instant::now() + HANG));
        let floor = ingest.floor(macs[0].0.mesh());
        assert!(
            floor > last,
            "the floor {floor} passes the conflict at {last}"
        );
        macs[0]
            .0
            .mesh()
            .cut_tables_for_test(floor)
            .expect("the cut");
        let mark = macs[0].0.seen.mark();
        macs[1].0.start(&lab);
        macs[1]
            .1
            .serve(&macs[1].0.mesh().egress().expect("an egress"));
        lab.tick();
        macs[0].0.seen.until("the relay reads the gap", |events| {
            events.iter().skip(mark).any(|event| {
                matches!(
                    event,
                    Event::Gap {
                        log: MeshLog::Tables,
                        ..
                    }
                )
            })
        });
        if gap_rule {
            macs[1].1.until_holds("while-asleep");
            macs[1].1.until_holds("a-conflict");
            assert!(macs[1].1.holds_kind(Kind::Conflict, "a-conflict"));
            macs[0].0.seen.until("A's exchange ends", |events| {
                events.iter().skip(mark).any(|event| {
                    matches!(
                        event,
                        Event::Inventory {
                            reason: Some(Reason::Gap),
                            ..
                        }
                    )
                })
            });
            let events = lock(&macs[0].0.seen.events).clone();
            let gaps = events
                .iter()
                .skip(mark)
                .filter(|event| {
                    matches!(
                        event,
                        Event::Inventory {
                            reason: Some(Reason::Gap),
                            ..
                        }
                    )
                })
                .count();
            assert_eq!(gaps, 1, "one Inventory filled the gap");
        } else {
            // A barrier: a later record reaches B over the relay in a later burst than the one
            // whose gap notice would have asked for an exchange, and A's Inventory worker then has
            // nothing asked or running.
            macs[0].1.write("w", "after");
            macs[1].1.until_holds("after");
            let inventory = macs[0].0.mesh().inventory().expect("an Inventory");
            assert!(inventory.wait_idle_for_test(Instant::now() + HANG));
            let events = lock(&macs[0].0.seen.events).clone();
            assert_eq!(started(&events, mark), 0);
            assert!(
                !macs[1].1.holds("while-asleep") && !macs[1].1.holds("a-conflict"),
                "the cut records arrived with no Inventory"
            );
        }
        crate::inventory::NO_INVENTORY_ON_GAP.with(|on| on.set(false));
    }
}

/// An `Inventory` that moved nothing is not run again with the same neighbour while this Mac's
/// digest is unchanged; with that rule off (the negative control), it is.
#[test]
fn an_exchange_that_moved_nothing_is_not_repeated() {
    for rule in [true, false] {
        let lab = Lab::new();
        let mut macs: Vec<(Mac, Arc<Fake>)> =
            (1..=2).map(|tag| tables_mac(&lab, tag, &["w"])).collect();
        start_all(&lab, &mut macs);
        let (a, b) = (&macs[0].0, &macs[1].0);
        let inventory = a.mesh().inventory().expect("an Inventory");
        inventory.quiet_off_for_test(!rule);
        let link = a
            .run
            .as_ref()
            .expect("A runs")
            .table
            .links()
            .into_iter()
            .find(|link| astream_broker::identity_principal(link.peer().key.bytes()) == b.principal)
            .expect("a link to B");
        let mark = a.seen.mark();
        inventory.request(&b.principal, &link, Reason::Gap);
        a.seen.until("the request is answered", |events| {
            events.iter().skip(mark).any(|event| {
                matches!(
                    event,
                    Event::Inventory { .. } | Event::InventorySkipped { .. }
                )
            })
        });
        let events = lock(&a.seen.events).clone();
        let ran = started(&events, mark);
        if rule {
            assert_eq!(ran, 0, "an exchange that moved nothing ran again");
        } else {
            assert_eq!(ran, 1);
        }
    }
}

/// The child process: append one record to the stand-in store in `CHILD_DIR` (a table operation,
/// or a membership record) and wake the node service whose state dir is `CHILD_STATE`, as a
/// CLI's write does.
#[test]
#[ignore = "child process fixture for one_append_yields_exactly_one_egress_record"]
fn child_append() {
    let dir = PathBuf::from(std::env::var_os(CHILD_DIR).expect("the store dir"));
    let state = PathBuf::from(std::env::var_os(CHILD_STATE).expect("the state dir"));
    let what = std::env::var(CHILD_WHAT).expect("what to append");
    let fake = Fake::new(&dir);
    let wake = if what == "member" {
        append_line(&fake.members_file(), &hex(b"seat:from-a-child"));
        Wake::Members {
            length: fake.members().len(),
        }
    } else {
        let file = fake.table_file(&TABLE);
        append_line(
            &file,
            &format!("{} {}", Kind::Operation.byte(), hex(b"w|from-a-child")),
        );
        Wake::Table {
            key: TableKey {
                table: TABLE,
                recovery: false,
            },
            length: lines(&file).len() as u64,
        }
    };
    crate::egress::wake(&state, &wake);
    println!("appended");
}

/// Run [`child_append`] for `what` against `mac`'s store.
fn append_from_a_child(mac: &Mac, fake: &Fake, what: &str) {
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
        .args([
            "--exact",
            "tests::tables::child_append",
            "--ignored",
            "--nocapture",
        ])
        .env(CHILD_DIR, &fake.dir)
        .env(CHILD_STATE, mac.dir.path())
        .env(CHILD_WHAT, what)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::inherit())
        .spawn()
        .expect("the child");
    aterm_uds::deadline::prints(&mut child, "appended", HANG);
    let status = child.wait().expect("the child ends");
    assert!(status.success());
}

/// One append to a table store, and one write of the membership log, each yield exactly one record
/// on `tables.log` under this Mac's owner segment: made in this process (the store's sink feeds
/// the egress), and made in a second process (its wake names the store's new length). Feeding the
/// same append again yields nothing more.
#[test]
fn one_append_yields_exactly_one_egress_record_in_process_and_from_a_second_process() {
    let lab = Lab::new();
    let (mut mac, fake) = tables_mac(&lab, 1, &["w"]);
    mac.read(seating(&[&mac]));
    mac.start(&lab);
    let egress = mac.mesh().egress().expect("an egress");
    fake.serve(&egress);
    let key = TableKey {
        table: TABLE,
        recovery: false,
    };
    let deadline = || Instant::now() + HANG;
    let members_at_start = fake.members().len();

    fake.write("w", "in-process");
    assert!(egress.wait_published(key, 1, deadline()));
    assert_eq!(own(&mac).len(), 1);
    // The same append fed again: the cursor is past it.
    let held = fake.table_held(key, 0, 1).expect("the record");
    egress.feed(Feed::Table {
        key,
        length: 1,
        record: Some((held[0].clone(), false)),
    });
    fake.append_member(b"seat:in-process");
    assert!(egress.wait_members_published(members_at_start + 1, deadline()));
    assert_eq!(
        own(&mac).len(),
        2,
        "one table record and one membership record"
    );

    append_from_a_child(&mac, &fake, "table");
    assert!(egress.wait_published(key, 2, deadline()));
    append_from_a_child(&mac, &fake, "member");
    assert!(egress.wait_members_published(members_at_start + 2, deadline()));
    let records = own(&mac);
    assert_eq!(records.len(), 4, "{records:?}");
    let subjects: BTreeSet<&str> = records.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(
        subjects,
        BTreeSet::from([
            stores::table_subject(&TABLE, &mac.principal).as_str(),
            stores::members_subject(&mac.principal).as_str(),
        ])
    );
    assert_eq!(egress.published(), 4);
}

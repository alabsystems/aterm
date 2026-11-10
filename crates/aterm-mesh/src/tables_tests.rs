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
use crate::logs::MESH_DIR;
use crate::presence::{self, Presence, PresenceConfig};
use crate::stores::{self, Digest, Held, Kind, ProofKey, Stores, TableKey, Verdict};
use crate::{Event, MeshLog};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Whether the stand-in fails to read its inventory, as a store whose read fails: an
    /// `Inventory` it answers ends before its greeting ([`Self::refuse`]).
    refuses: AtomicBool,
    /// The tables the stand-in does not take, as a vault whose Commons policy excludes them:
    /// their records are refused, and an `Inventory` is given none ([`Self::exclude`]).
    excludes: Mutex<BTreeSet<Digest>>,
}

/// Every stand-in store file is read and written under this, in this process: a store's own writes
/// (the test's thread) and its ingest (the mesh's) append to one file at once, and an append counts
/// the file's records under it ([`append_line`]).
static FILES: Mutex<()> = Mutex::new(());

/// The records of a stand-in store file: its newline-terminated lines, read under [`FILES`]. A child
/// process appends under no lock of this one ([`child_append`]), so a line still being written (no
/// newline yet) is no record until it is whole.
fn lines(path: &Path) -> Vec<String> {
    let _files = lock(&FILES);
    std::fs::read_to_string(path)
        .map(|text| {
            text.split_inclusive('\n')
                .filter_map(|line| line.strip_suffix('\n'))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Append `line` to the stand-in file at `path` as one write, its newline included (`writeln!` on a
/// `File` writes the line and its newline apart, so two appends at once could join two records into
/// one line): how many lines it holds then, counted under the same guard, so another append cannot
/// land between the two and give both one length.
fn append_line(path: &Path, line: &str) -> u64 {
    let _files = lock(&FILES);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("a stand-in store file");
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    file.write_all(&bytes).expect("a stand-in record");
    // Its records are its newline-terminated lines, as [`lines`] reads them.
    std::fs::read(path)
        .expect("the stand-in store file")
        .iter()
        .filter(|byte| **byte == b'\n')
        .count() as u64
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
            refuses: AtomicBool::new(false),
            excludes: Mutex::new(BTreeSet::new()),
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

    /// Feed no egress: an append after this is one whose feed the egress never reads.
    fn unserve(&self) {
        *lock(&self.egress) = None;
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
        self.until(text, |fake| fake.holds(text));
    }

    /// Wait until `done` holds of this store, `what` naming it.
    fn until(&self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + HANG;
        let mut changed = lock(&self.changed);
        while !done(self) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "{} did not come to hold {what:?} within the hang detector",
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
        let length = append_line(&file, &format!("{} {}", kind.byte(), hex(wire)));
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

    /// Fail (`on`) or serve every read of this stand-in's inventory.
    fn refuse(&self, on: bool) {
        self.refuses.store(on, Ordering::Release);
    }

    /// Take no record of `table` from now on.
    fn exclude(&self, table: Digest) {
        lock(&self.excludes).insert(table);
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

    /// Forget the operation whose text is `text`: a store that lost the last records its log
    /// had answered, as one whose Mac stopped before it ingested them does.
    fn forget(&self, text: &str) {
        let file = self.table_file(&TABLE);
        let tail = format!("|{text}");
        let kept: String = lines(&file)
            .into_iter()
            .filter(|line| {
                line.split_once(' ')
                    .is_none_or(|(_, wire)| !unhex(wire).ends_with(tail.as_bytes()))
            })
            .map(|line| line + "\n")
            .collect();
        {
            // A reader racing the rewrite (a child process takes no lock of this one) reads the
            // file before or after it, never a cut of it.
            let _files = lock(&FILES);
            let next = file.with_extension("next");
            std::fs::write(&next, kept).expect("the stand-in store file");
            std::fs::rename(&next, &file).expect("the stand-in store file");
        }
        self.bump();
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
        if lock(&self.excludes).contains(&table) {
            return vec![
                Verdict::Refused("the stand-in does not take this table".to_owned());
                wires.len()
            ];
        }
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

    fn takes(&self, table: Digest) -> io::Result<bool> {
        Ok(!lock(&self.excludes).contains(&table))
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
        if self.refuses.load(Ordering::Acquire) {
            return Err(io::Error::other(
                "the stand-in refuses to read its inventory",
            ));
        }
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
    start_linked(lab, macs, |_, _| true);
}

/// [`start_all`] where Macs `i < j` are neighbours only where `linked(i, j)`: the lab keeps every
/// other pair apart.
fn start_linked(lab: &Lab, macs: &mut [(Mac, Arc<Fake>)], linked: impl Fn(usize, usize) -> bool) {
    let neighbours = |i: usize, j: usize| i != j && linked(i.min(j), i.max(j));
    for i in 0..macs.len() {
        for j in i + 1..macs.len() {
            if !neighbours(i, j) {
                lab.keep_apart(&macs[i].0, &macs[j].0);
            }
        }
    }
    let all = seating(&macs.iter().map(|(mac, _)| mac).collect::<Vec<_>>());
    for (mac, fake) in macs.iter_mut() {
        mac.read(all.clone());
        mac.start(lab);
        fake.serve(&mac.mesh().egress().expect("the mesh moves tables"));
    }
    for (i, (mac, _)) in macs.iter().enumerate() {
        let peers: Vec<&Mac> = macs
            .iter()
            .enumerate()
            .filter(|(j, _)| neighbours(i, *j))
            .map(|(_, (m, _))| m)
            .collect();
        mac.until_following(&peers, 0);
        mac.seen
            .until("the first Inventory of each link", |events| {
                started(events, 0) + skipped(events) >= peers.len()
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
/// own records, the operations `table_inventory` lists ([`crate::presence::Held::table`]) and the
/// conflict records ([`crate::presence::Held::conflicts`]), the same on every Mac.
fn settled_tables(macs: &[(Mac, Arc<Fake>)]) -> crate::presence::Held {
    use crate::presence::Held as Holding;
    let key = TableKey {
        table: TABLE,
        recovery: false,
    };
    let held: Vec<Holding> = macs
        .iter()
        .map(|(mac, fake)| {
            let length = fake.records(&TABLE).len() as u64;
            let egress = mac.mesh().egress().expect("an egress");
            assert!(egress.wait_published(key, length, Instant::now() + HANG));
            let operations = fake
                .table_inventory(TABLE)
                .expect("the store reads")
                .expect("the store holds the table");
            let conflicts: BTreeSet<Digest> = fake
                .records(&TABLE)
                .iter()
                .filter(|(kind, _)| *kind == Kind::Conflict)
                .map(|(_, wire)| stores::digest(wire))
                .collect();
            let of = |ids: &BTreeSet<Digest>, summary: fn(&BTreeSet<Digest>) -> Holding| {
                (!ids.is_empty())
                    .then(|| (TABLE, summary(ids)))
                    .into_iter()
                    .collect::<BTreeMap<Digest, Holding>>()
            };
            let held = mac
                .mesh()
                .tables_held()
                .expect("the first catch-up read every store");
            assert_eq!(
                held,
                Holding::tables(
                    &of(&operations, Holding::table),
                    &of(&conflicts, Holding::conflicts)
                ),
                "{} reads what its store holds",
                mac.principal
            );
            held
        })
        .collect();
    assert!(held.windows(2).all(|pair| pair[0] == pair[1]), "{held:?}");
    held[0]
}

/// Macs that hold the same records read the same tables digest in their presence rows (§10's
/// soak: "every instance's presence row shows the same tables digest"), and it is what their
/// stores hold: an operation from before the mesh counts (the first catch-up read it, no sink fed
/// it), as does one the mesh delivered; a write moves it on every Mac once it projects there, and
/// so does a conflict record, which an `Inventory` moves too (PF5b's finding H1).
#[test]
fn macs_that_hold_the_same_records_read_the_same_tables_digest() {
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
    let conflicted = settled_tables(&macs);
    assert_eq!(conflicted.count, 3, "two operations and a conflict record");
    assert_ne!(
        conflicted.digest, written.digest,
        "a conflict record moves the digest"
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

/// A table a Mac never held, first written and cut from every log while it was away, reaches it
/// by the `Inventory` its return starts, which gives it a table only the other side holds; a
/// table its stores do not take is given it by none. With `BOTH_HOLD_ONLY` on (the rule before
/// §11 B8), the exchange runs and moves nothing.
#[test]
fn a_table_first_written_and_cut_while_a_mac_was_away_reaches_it_by_inventory() {
    /// A table B's stores do not take.
    const OTHER: Digest = [0x6b; 32];
    for one_sided in [true, false] {
        crate::inventory::BOTH_HOLD_ONLY.with(|on| on.set(!one_sided));
        let lab = Lab::new();
        let mut macs: Vec<(Mac, Arc<Fake>)> =
            (1..=2).map(|tag| tables_mac(&lab, tag, &["w"])).collect();
        start_all(&lab, &mut macs);
        macs[1].1.exclude(OTHER);
        // B goes away before either table has a record, so it never held one; A writes both, and
        // its log, the only one that holds them, is cut past them.
        macs[1].0.stop();
        macs[0].1.write("w", "first");
        macs[0]
            .1
            .append(&TABLE, Kind::Conflict, b"w|a-conflict", false);
        macs[0]
            .1
            .append(&OTHER, Kind::Operation, b"w|not-taken", false);
        let egress = macs[0].0.mesh().egress().expect("an egress");
        for (table, length) in [(TABLE, 2), (OTHER, 1)] {
            let key = TableKey {
                table,
                recovery: false,
            };
            assert!(egress.wait_published(key, length, Instant::now() + HANG));
        }
        let last = [TABLE, OTHER]
            .iter()
            .map(|table| egress.last_published(table).expect("A published it"))
            .max()
            .expect("two tables");
        let ingest = macs[0].0.mesh().ingest().expect("an ingest consumer");
        assert!(ingest.wait_committed_for_test(last, Instant::now() + HANG));
        let floor = ingest.floor(macs[0].0.mesh());
        assert!(floor > last, "the floor {floor} passes the last at {last}");
        macs[0]
            .0
            .mesh()
            .cut_tables_for_test(floor)
            .expect("the cut");
        assert!(!log_holds(&macs[0].0, "first"), "the cut passed the table");
        let mark = macs[0].0.seen.mark();
        macs[1].0.start(&lab);
        macs[1]
            .1
            .serve(&macs[1].0.mesh().egress().expect("an egress"));
        lab.tick();
        // A's relay reads the gap and asks the exchange. A reports it once it read B's batches,
        // which B sends after it took every record A gave it.
        macs[0].0.seen.until("A's gap exchange ends", |events| {
            started_for(events, mark, Reason::Gap) >= 1
        });
        for (mac, _) in &macs {
            let inventory = mac.mesh().inventory().expect("an Inventory");
            assert!(inventory.wait_idle_for_test(Instant::now() + HANG));
        }
        let held = |fake: &Fake, table: &Digest| -> BTreeSet<(Kind, Vec<u8>)> {
            fake.records(table).into_iter().collect()
        };
        if one_sided {
            assert_eq!(
                held(&macs[1].1, &TABLE),
                held(&macs[0].1, &TABLE),
                "B holds every record of the table it never held"
            );
            // Concurrent reverse-direction Inventory can carry a proof before the
            // Gap exchange reaches that unit. Its responder event has no reason;
            // a Gap-only wire count therefore does not measure final delivery.
            assert_eq!(
                held(&macs[1].1, &TABLE),
                BTreeSet::from([
                    (Kind::Operation, b"w|first".to_vec()),
                    (Kind::Conflict, b"w|a-conflict".to_vec()),
                ]),
                "B received exactly the two original records, regardless of exchange initiator"
            );
            assert!(
                !log_holds(&macs[1].0, "first"),
                "the record reached B by the exchange, in no log of B's"
            );
        } else {
            assert!(held(&macs[1].1, &TABLE).is_empty());
        }
        assert!(
            held(&macs[1].1, &OTHER).is_empty() && !macs[1].1.table_file(&OTHER).exists(),
            "B was given a table its stores do not take"
        );
        crate::inventory::BOTH_HOLD_ONLY.with(|on| on.set(false));
    }
}

/// The offset of the table operation on `mac`'s `tables.log` whose text is `text`.
fn offset_of(mac: &Mac, text: &str) -> u64 {
    offset_carrying(mac, format!("|{text}").as_bytes())
        .unwrap_or_else(|| panic!("{}'s log holds no {text:?}", mac.principal))
}

/// The offset of the first record on `mac`'s `tables.log` whose body carries `needle`.
fn offset_carrying(mac: &Mac, needle: &[u8]) -> Option<u64> {
    let mut client = mac
        .mesh()
        .read_client(MeshLog::Tables, "/t/>", Some(HANG))
        .expect("a reader");
    let mut from = 0;
    loop {
        let (page, (next, head)) = client.fetch(from, "/t/>", 1024).expect("a page");
        if let Some((offset, _, _)) = page
            .iter()
            .find(|(_, _, body)| body.windows(needle.len()).any(|w| w == needle))
        {
            return Some(*offset);
        }
        if next >= head {
            return None;
        }
        from = next;
    }
}

/// The `Inventory` exchanges a Mac started for `reason` from event `since` on.
fn started_for(events: &[Event], since: usize, reason: Reason) -> usize {
    events
        .iter()
        .skip(since)
        .filter(|event| matches!(event, Event::Inventory { reason: Some(r), .. } if *r == reason))
        .count()
}

/// Rot the newest batch of the stopped log at `file`: its last byte, inside the newest record's
/// frame, flipped. The next open cannot tell rot in the newest batch from a torn write, so it
/// cuts that batch and announces the cut on every tier: the log's generation moves, with lineage
/// at the head it keeps (astream's `recover.rs`, "What 'written' means for a cut").
fn rot_the_newest_batch(file: &Path) {
    use std::os::unix::fs::FileExt as _;
    let log = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(file)
        .expect("the stopped log");
    let last = log
        .metadata()
        .expect("the log's length")
        .len()
        .checked_sub(1)
        .expect("a log that holds records");
    let mut byte = [0u8];
    log.read_exact_at(&mut byte, last)
        .expect("the log's last byte");
    log.write_all_at(&[!byte[0]], last).expect("the rot");
}

/// The lineage event (`relay_live_island.rs`'s `lineage_repaired`): a neighbour whose log answered
/// a record and whose stores lost it before they held it for good comes back in another boot, at a
/// later generation of its log with lineage. The relay to it reads the cursor moved and asks the
/// lineage exchange, which brings the record back. With `KEPT_ON_A_MOVED_LOG` (the island's
/// `keptOnMove`), the relay resumes as if the log were the same, asks nothing, and the record
/// never returns. The log moves for the unclean stop where it served a weak tier (Ordered, on
/// Apple); where it served Strict (Ordered off Apple), whose acks were on stable media, it moves
/// for its newest batch, which rots while B is away ([`rot_the_newest_batch`]).
#[test]
fn a_neighbour_that_lost_what_it_answered_gets_it_back_by_the_lineage_exchange() {
    for lineage_rule in [true, false] {
        crate::relay::KEPT_ON_A_MOVED_LOG.with(|on| on.set(!lineage_rule));
        let lab = Lab::new();
        let mut macs: Vec<(Mac, Arc<Fake>)> =
            (1..=2).map(|tag| tables_mac(&lab, tag, &["w"])).collect();
        start_all(&lab, &mut macs);
        crate::relay::KEPT_ON_A_MOVED_LOG.with(|on| on.set(false));
        macs[0].1.write("w", "before");
        macs[1].1.until_holds("before");
        macs[0].1.write("w", "answered");
        macs[1].1.until_holds("answered");
        // B's ingest group steps past the record, so B's own log never hands it to its stores
        // again; then B stops uncleanly and its stores lose it.
        let at = offset_of(&macs[1].0, "answered");
        let ingest = macs[1].0.mesh().ingest().expect("an ingest consumer");
        assert!(ingest.wait_committed_for_test(at, Instant::now() + HANG));
        let tables = macs[1].0.mesh().broker(MeshLog::Tables);
        let was = tables.log_identity();
        // Where its unclean stop moves nothing (Strict), B's newest batch rots instead, and the
        // open cuts it. A later record and B's commit past it come first, so the cut keeps every
        // record through `spare`: at most B's ingest loses its commit past `spare` and reads it
        // again, which its stores hold.
        let rots = (tables.served_durability().served == astream_broker::Tier::Strict).then(|| {
            macs[0].1.write("w", "spare");
            macs[1].1.until_holds("spare");
            let spare = offset_of(&macs[1].0, "spare");
            assert!(ingest.wait_committed_for_test(spare, Instant::now() + HANG));
            macs[1].0.mesh().log_file(MeshLog::Tables)
        });
        for log in MeshLog::ALL {
            macs[1].0.mesh().broker(log).arm_unclean_stop();
        }
        let since = macs[0].0.seen.mark();
        macs[1].0.stop();
        let gone = macs[1].0.principal.clone();
        macs[0].0.seen.until("A's relay to B ended", |events| {
            events.iter().skip(since).any(
                |e| matches!(e, Event::Ended { peer, log: MeshLog::Tables, .. } if *peer == gone),
            )
        });
        macs[1].1.forget("answered");
        assert!(!macs[1].1.holds("answered"));
        if let Some(file) = &rots {
            rot_the_newest_batch(file);
        }
        let mark = macs[0].0.seen.mark();
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    crate::logs::BOOT.with(|boot| boot.set(Some([7; 16])));
                    macs[1].0.start(&lab);
                })
                .join()
        })
        .expect("B starts again, in another boot");
        let now = macs[1].0.mesh().broker(MeshLog::Tables).log_identity();
        assert!(
            now.id == was.id && now.generation > was.generation,
            "the reboot moved {was} to {now}"
        );
        macs[1]
            .1
            .serve(&macs[1].0.mesh().egress().expect("an egress"));
        lab.tick();
        if lineage_rule {
            macs[1].1.until_holds("answered");
            macs[0].0.seen.until("A's lineage exchange ends", |events| {
                started_for(events, mark, Reason::Lineage) > 0
            });
        } else {
            // A barrier: a later record reaches B over the resumed relay, and A's Inventory worker
            // then has nothing asked or running.
            macs[0].1.write("w", "after");
            macs[1].1.until_holds("after");
            let inventory = macs[0].0.mesh().inventory().expect("an Inventory");
            assert!(inventory.wait_idle_for_test(Instant::now() + HANG));
            let events = lock(&macs[0].0.seen.events).clone();
            assert_eq!(started_for(&events, mark, Reason::Lineage), 0);
            assert!(
                !macs[1].1.holds("answered"),
                "the lost record came back with no lineage exchange"
            );
        }
    }
}

/// Whether `mac`'s `tables.log` holds the table operation whose text is `text`, of what it keeps:
/// a cut leaves the log complete only from its oldest, and a fetch below that is refused.
fn log_holds(mac: &Mac, text: &str) -> bool {
    let tail = format!("|{text}");
    let mut client = mac
        .mesh()
        .read_client(MeshLog::Tables, "/t/>", Some(HANG))
        .expect("a reader");
    let mut from = client.oldest("/t/>").expect("the oldest").0.unwrap_or(0);
    loop {
        let (page, (next, head)) = client.fetch(from, "/t/>", 1024).expect("a page");
        if page
            .iter()
            .any(|(_, _, body)| body.windows(tail.len()).any(|w| w == tail.as_bytes()))
        {
            return true;
        }
        if next >= head {
            return false;
        }
        from = next;
    }
}

/// A Mac's presence, its rows naming what its membership log holds (as the node service's read
/// its records file) and what its lab tables hold as its egress read them (as the node service's
/// do), and what it reported.
struct Rows {
    presence: Presence,
    heard: Arc<(Mutex<Vec<presence::Event>>, Condvar)>,
}

impl Rows {
    /// Start `mac`'s presence over its store `fake`, as its node service does, and wait until its
    /// tail runs and its first row is out (so the next period is the one its next row waits for).
    fn start(mac: &Mac, fake: &Arc<Fake>) -> Self {
        let mesh = Arc::downgrade(mac.mesh());
        let store = Arc::clone(fake);
        let heard: Arc<(Mutex<Vec<presence::Event>>, Condvar)> = Arc::default();
        let noted = Arc::clone(&heard);
        let presence = Presence::start(
            mac.mesh(),
            PresenceConfig {
                dir: mac.dir.path().join(MESH_DIR),
                facts: Arc::new(move || presence::Facts {
                    build: presence::Build::new("0.155.0+gdigest").expect("a build"),
                    addresses: Vec::new(),
                    membership: store
                        .members_inventory()
                        .ok()
                        .map(|ids| presence::Held::membership(&ids)),
                    tables: mesh.upgrade().and_then(|mesh| mesh.tables_held()),
                    topics: Vec::new(),
                }),
                observe: Some(Arc::new(move |event: &presence::Event| {
                    lock(&noted.0).push(event.clone());
                    noted.1.notify_all();
                })),
            },
        )
        .expect("presence starts");
        let rows = Self { presence, heard };
        let deadline = Instant::now() + HANG;
        let mut heard = lock(&rows.heard.0);
        while !(heard
            .iter()
            .any(|e| matches!(e, presence::Event::Tailing { .. }))
            && heard
                .iter()
                .any(|e| matches!(e, presence::Event::Published { .. })))
        {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "{}'s presence did not start within the hang detector",
                mac.principal
            );
            heard = rows
                .heard
                .1
                .wait_timeout(heard, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        drop(heard);
        rows
    }

    /// Wait until this Mac holds the row `of` wrote at wall time `at_ms` and reported taking it:
    /// the digest trigger read it then.
    fn until_took(&self, of: &Mac, at_ms: u64) {
        let deadline = Instant::now() + HANG;
        let mut heard = lock(&self.heard.0);
        loop {
            let row = self
                .presence
                .seat(&of.principal)
                .and_then(|seat| seat.row)
                .filter(|(_, row)| row.at_ms == at_ms);
            if let Some((seq, _)) = row
                && heard.iter().any(|event| {
                    matches!(event, presence::Event::Took { principal, seq: s, .. }
                        if *principal == of.principal && *s == seq)
                })
            {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "no row of {} written at {at_ms} was taken within the hang detector",
                of.principal
            );
            heard = self
                .heard
                .1
                .wait_timeout(heard, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

/// What a digest-trigger run cuts below sleeping B's cursor: a record of one unit an `Inventory`
/// moves, so each unit's digest is held to carrying it on.
#[derive(Clone, Copy, Debug)]
enum Cut {
    /// A table operation: the tables digest's operations.
    Operation,
    /// A conflict certificate: the tables digest's conflict records.
    Conflict,
    /// A membership record: the membership digest.
    Member,
}

impl Cut {
    /// A's local write of it.
    fn write(self, fake: &Fake) {
        match self {
            Self::Operation => fake.write("w", "while-asleep"),
            Self::Conflict => fake.append(&TABLE, Kind::Conflict, b"w|while-asleep", false),
            Self::Member => fake.append_member(b"seat:while-asleep"),
        }
    }

    /// Whether `fake` holds it.
    fn held(self, fake: &Fake) -> bool {
        match self {
            Self::Operation => fake.holds_kind(Kind::Operation, "while-asleep"),
            Self::Conflict => fake.holds_kind(Kind::Conflict, "while-asleep"),
            Self::Member => fake.seated("while-asleep"),
        }
    }

    /// Wait until `fake` holds it.
    fn until_held(self, fake: &Fake) {
        fake.until(&format!("the {self:?} record"), |fake| self.held(fake));
    }

    /// What a `tables.log` record of it carries.
    const fn needle(self) -> &'static [u8] {
        match self {
            Self::Operation | Self::Conflict => b"|while-asleep",
            Self::Member => b"seat:while-asleep",
        }
    }

    /// Wait until A's egress published it (A's store holding it and `before`).
    fn until_published(self, a: &(Mac, Arc<Fake>)) {
        let egress = a.0.mesh().egress().expect("an egress");
        let deadline = Instant::now() + HANG;
        let published = match self {
            Self::Operation | Self::Conflict => {
                let key = TableKey {
                    table: TABLE,
                    recovery: false,
                };
                egress.wait_published(key, 2, deadline)
            }
            Self::Member => egress.wait_members_published(a.1.members().len(), deadline),
        };
        assert!(published, "A published the {self:?} record");
    }
}

/// H1 closed (`relay_live_island.rs`'s `h1_hole` and `h1_digest_closes_it`;
/// `relay_digest_island.rs`), for every unit an exchange moves: on a line of Macs A - B - C, a
/// record cut from A's log below sleeping B's cursor reaches B by the gap's `Inventory`, which puts
/// it in B's stores and in no log, so B's relay owes C nothing of it. Each Mac's presence rows then
/// show a digest of that record's store other than the other's, two rows in a row, both relays
/// caught up, and the digest trigger asks the exchange that brings the record to C. With
/// `NO_DIGEST_TRIGGER` on, it never reaches C.
fn a_record_repaired_to_a_neighbour_reaches_the_next_hop(cut: Cut) {
    for trigger in [true, false] {
        crate::inventory::NO_DIGEST_TRIGGER.with(|on| on.set(!trigger));
        let lab = Lab::new();
        let mut macs: Vec<(Mac, Arc<Fake>)> =
            (1..=3).map(|tag| tables_mac(&lab, tag, &["w"])).collect();
        start_linked(&lab, &mut macs, |i, j| j == i + 1);
        macs[0].1.write("w", "before");
        macs[2].1.until_holds("before");
        // B sleeps; A writes, and its log is cut past what B's cursor holds.
        macs[1].0.stop();
        cut.write(&macs[0].1);
        cut.until_published(&macs[0]);
        let last = offset_carrying(&macs[0].0, cut.needle()).expect("A's log holds the record");
        let ingest = macs[0].0.mesh().ingest().expect("an ingest consumer");
        assert!(ingest.wait_committed_for_test(last, Instant::now() + HANG));
        let floor = ingest.floor(macs[0].0.mesh());
        assert!(floor > last, "the floor {floor} passes the write at {last}");
        macs[0]
            .0
            .mesh()
            .cut_tables_for_test(floor)
            .expect("the cut");
        // B wakes cut off, so its store serves its egress before an exchange lands in it, then
        // links again: A's relay reads the gap, and its exchange brings the record to B.
        let marks: Vec<usize> = macs.iter().map(|(mac, _)| mac.seen.mark()).collect();
        lock(&lab.cut).insert(macs[1].0.principal.clone());
        macs[1].0.start(&lab);
        macs[1]
            .1
            .serve(&macs[1].0.mesh().egress().expect("an egress"));
        macs[1].0.heal(&lab);
        cut.until_held(&macs[1].1);
        macs[0].0.seen.until("A's gap exchange ends", |events| {
            started_for(events, marks[0], Reason::Gap) > 0
        });
        macs[1]
            .0
            .until_following(&[&macs[0].0, &macs[2].0], marks[1]);
        macs[2].0.until_following(&[&macs[1].0], marks[2]);
        // The hole: the record is in B's stores and its egress's digest (which names what A's
        // does), in no log B's relays read, and not on C.
        settled_tables(&macs[..2]);
        assert_eq!(offset_carrying(&macs[1].0, cut.needle()), None);
        assert!(!cut.held(&macs[2].1));

        // Two rows of each, both read by the other: the digests differ, steady, the relays follow.
        let rows: Vec<Rows> = [&macs[2], &macs[1]]
            .into_iter()
            .map(|(mac, fake)| Rows::start(mac, fake))
            .collect();
        let (b, c) = (&macs[1].0, &macs[2].0);
        for _ in 0..2 {
            lab.age(u64::try_from(presence::EVERY.as_millis()).expect("a period"));
            lab.clock.advance(presence::EVERY);
            let at = lab.wall.load(Ordering::Acquire);
            for reader in &rows {
                for of in [b, c] {
                    reader.until_took(of, at);
                }
            }
        }
        // Each row above asked what it asks before it was reported; the exchanges it asked end
        // before B's and C's Inventory workers are idle.
        let settle = |m: usize| {
            let inventory = macs[m].0.mesh().inventory().expect("an Inventory");
            assert!(inventory.wait_idle_for_test(Instant::now() + HANG));
            let events = lock(&macs[m].0.seen.events).clone();
            (
                started(&events, marks[m]),
                started_for(&events, marks[m], Reason::Digest),
            )
        };
        if trigger {
            cut.until_held(&macs[2].1);
            assert_eq!(
                offset_carrying(&macs[2].0, cut.needle()),
                None,
                "the record reached C by an exchange, not by a relay"
            );
            let mut asked = 0;
            for m in [1, 2] {
                let (all, digest) = settle(m);
                assert_eq!(
                    all, digest,
                    "{} started only digest exchanges",
                    macs[m].0.principal
                );
                asked += digest;
            }
            assert!(asked > 0, "no digest exchange ran");
        } else {
            // A barrier: a later record reaches C over both relays, after every row above, and
            // B's and C's Inventory workers then have nothing asked or running.
            macs[0].1.write("w", "after");
            macs[2].1.until_holds("after");
            for m in [1, 2] {
                assert_eq!(
                    settle(m),
                    (0, 0),
                    "{} started an exchange",
                    macs[m].0.principal
                );
            }
            assert!(
                !cut.held(&macs[2].1),
                "the record reached C with the digest trigger off"
            );
        }
        drop(rows);
        crate::inventory::NO_DIGEST_TRIGGER.with(|on| on.set(false));
    }
}

/// H2 (`relay_live_island.rs`'s `h2Run`): B's log holds a record its stores have not yet ingested
/// when its relay to a new neighbour C starts at the head, so the new link's exchange, listing B's
/// stores, moves nothing of it, and no relay carries it. The start asked again for when B's ingest
/// passes the last table record its log held (`relay_live::asks_again`), and that exchange brings
/// it. Under `NO_SECOND_ASK` (the island's `noSecondAsk`) nothing asks again, and C never gets it.
/// B's ingest settles the record either when it is let go (a test hold) or, `by_retry`, when its
/// writer's seat reaches B's stores and the waiting record applies on its retry, with nothing new
/// delivered and no commit to take the floor past it.
#[test]
fn a_record_a_relay_start_had_not_ingested_reaches_the_new_neighbour_by_the_second_ask() {
    for (second_ask, by_retry) in [(true, false), (false, false), (true, true), (false, true)] {
        crate::inventory::NO_SECOND_ASK.with(|on| on.set(!second_ask));
        let lab = Lab::new();
        // C seats both writers; A and B only `w`, so a record of `late` waits in B's ingest.
        let mut macs: Vec<(Mac, Arc<Fake>)> = (1..=3)
            .map(|tag| tables_mac(&lab, tag, if tag == 3 { &["w", "late"] } else { &["w"] }))
            .collect();
        let writer = if by_retry { "late" } else { "w" };
        // A and B run, seating C, which starts later.
        let all = seating(&macs.iter().map(|(mac, _)| mac).collect::<Vec<_>>());
        for (mac, fake) in &mut macs[..2] {
            mac.read(all.clone());
            mac.start(&lab);
            fake.serve(&mac.mesh().egress().expect("the mesh moves tables"));
        }
        macs[0].0.until_following(&[&macs[1].0], 0);
        macs[1].0.until_following(&[&macs[0].0], 0);
        for (mac, _) in &macs[..2] {
            mac.seen.until("the first Inventory of the link", |events| {
                started(events, 0) + skipped(events) >= 1
            });
        }
        // B's ingest holds what it reads, or the record waits for its writer's seat: A's record
        // lands in B's log, not in its stores.
        let ingest = macs[1].0.mesh().ingest().expect("an ingest consumer");
        ingest.hold_for_test(!by_retry);
        let b = macs[1].0.principal.clone();
        let mark = macs[0].0.seen.mark();
        macs[0].1.write(writer, "unsettled");
        macs[0]
            .0
            .seen
            .until("A's relay lands it in B's log", |events| {
                events.iter().skip(mark).any(|e| {
                    matches!(e, Event::Burst { peer, log: MeshLog::Tables, appended, .. }
                    if *peer == b && *appended > 0)
                })
            });
        let at = offset_of(&macs[1].0, "unsettled");
        if by_retry {
            // A commit passes a waiting record: the group stands past it, the floor at it.
            assert!(ingest.wait_committed_for_test(at, Instant::now() + HANG));
        }
        assert!(
            ingest.floor(macs[1].0.mesh()) <= at,
            "B's ingest settled nothing of it"
        );
        assert!(!macs[1].1.holds("unsettled"));
        // A leaves, so only B can bring it to C; C starts, and B's relay to it, with no cursor,
        // starts at B's head, past the record. Both new link's exchanges end before B ingests.
        macs[0].0.stop();
        let (mark_b, mark_c) = (macs[1].0.seen.mark(), macs[2].0.seen.mark());
        macs[2].0.read(all.clone());
        macs[2].0.start(&lab);
        macs[2]
            .1
            .serve(&macs[2].0.mesh().egress().expect("an egress"));
        lab.tick();
        macs[1].0.until_following(&[&macs[2].0], mark_b);
        let c = macs[2].0.principal.clone();
        // Each side's new link request runs, or is skipped once the other's moved nothing.
        macs[1]
            .0
            .seen
            .until("B's new link request with C is answered", |events| {
                events.iter().skip(mark_b).any(|e| {
                    matches!(e, Event::Inventory { peer, reason: Some(Reason::NewLink), .. }
                    | Event::InventorySkipped { peer, reason: Reason::NewLink }
                    if *peer == c)
                })
            });
        macs[2]
            .0
            .seen
            .until("C's new link request with B is answered", |events| {
                events.iter().skip(mark_c).any(|e| {
                    matches!(e, Event::Inventory { peer, reason: Some(Reason::NewLink), .. }
                    | Event::InventorySkipped { peer, reason: Reason::NewLink }
                    if *peer == b)
                })
            });
        assert!(
            !macs[2].1.holds("unsettled"),
            "the first exchange brought what B's stores lacked"
        );
        // C holds the table (an exchange lists only the tables both sides hold), by a record the
        // mesh delivered, which its egress does not publish again.
        macs[2].1.append(&TABLE, Kind::Operation, b"w|seed", true);
        let released = macs[1].0.seen.mark();
        if by_retry {
            // The seat reaches B's stores by another way than its log (as an exchange of the
            // membership log or a membership read brings it), and the mesh kicks the ingest.
            macs[1].1.seed_seat("late");
            ingest.kick();
        } else {
            ingest.hold_for_test(false);
        }
        if second_ask {
            macs[2].1.until_holds("unsettled");
            macs[1]
                .0
                .seen
                .until("B's second exchange with C ends", |events| {
                    started_for(events, released, Reason::NewLink) > 0
                });
        } else {
            // A barrier: B's ingest settles the record, a later record of B's reaches C over the
            // relay, and B's Inventory worker then has nothing asked or running.
            assert!(ingest.wait_committed_for_test(at, Instant::now() + HANG));
            macs[1].1.until_holds("unsettled");
            macs[1].1.write("w", "after");
            macs[2].1.until_holds("after");
            let inventory = macs[1].0.mesh().inventory().expect("an Inventory");
            assert!(inventory.wait_idle_for_test(Instant::now() + HANG));
            let events = lock(&macs[1].0.seen.events).clone();
            assert_eq!(started_for(&events, released, Reason::NewLink), 0);
            assert!(
                !macs[2].1.holds("unsettled"),
                "the record reached C with no second ask"
            );
        }
        crate::inventory::NO_SECOND_ASK.with(|on| on.set(false));
    }
}

/// [`a_record_repaired_to_a_neighbour_reaches_the_next_hop`] for a table operation.
#[test]
fn a_record_repaired_to_a_neighbour_reaches_the_next_hop_by_the_digest_trigger() {
    a_record_repaired_to_a_neighbour_reaches_the_next_hop(Cut::Operation);
}

/// [`a_record_repaired_to_a_neighbour_reaches_the_next_hop`] for a conflict certificate, which
/// the tables digest names beside the operations.
#[test]
fn a_conflict_certificate_repaired_to_a_neighbour_reaches_the_next_hop() {
    a_record_repaired_to_a_neighbour_reaches_the_next_hop(Cut::Conflict);
}

/// [`a_record_repaired_to_a_neighbour_reaches_the_next_hop`] for a membership record, which the
/// membership digest names.
#[test]
fn a_membership_record_repaired_to_a_neighbour_reaches_the_next_hop() {
    a_record_repaired_to_a_neighbour_reaches_the_next_hop(Cut::Member);
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
            // The pretty single-threaded formatter prefixes the proof line with the test name.
            "--quiet",
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

/// A record the mesh delivered whose feed the egress never read (dropped from a full queue, or
/// the node stopped first) is not published as this Mac's when the catch-up reads it from its
/// store: its id was noted on disk before the append ([`crate::egress::ARRIVED_FILE`]). With the
/// note gone (the negative control), the catch-up publishes it as this Mac's.
#[test]
fn a_delivered_record_a_catch_up_reads_is_not_published_again() {
    for noted in [true, false] {
        let lab = Lab::new();
        let mut macs: Vec<(Mac, Arc<Fake>)> =
            (1..=2).map(|tag| tables_mac(&lab, tag, &["w"])).collect();
        start_all(&lab, &mut macs);
        // B's store feeds its egress nothing: the record's feed is lost.
        macs[1].1.unserve();
        macs[0].1.write("w", "feed-lost");
        macs[1].1.until_holds("feed-lost");
        let dir = macs[1].0.mesh().log_dir(MeshLog::Tables);
        macs[1].0.stop();
        if !noted {
            std::fs::remove_file(dir.join(crate::egress::ARRIVED_FILE)).expect("the note");
        }
        // The next start catches the store up from its cursor, below the record.
        macs[1].0.start(&lab);
        let egress = macs[1].0.mesh().egress().expect("an egress");
        macs[1].1.serve(&egress);
        let key = TableKey {
            table: TABLE,
            recovery: false,
        };
        assert!(egress.wait_published(key, 1, Instant::now() + HANG));
        assert_eq!(
            own(&macs[1].0).len(),
            usize::from(!noted),
            "noted: {noted}: what the catch-up published as B's"
        );
    }
}

/// A push another process of the Mac asks of the node service ([`crate::push::push_by_node`]) is
/// answered once a neighbour acked the table on the relay. Where no node serves the state dir,
/// nothing answers, and the asker's old path pushes.
#[test]
fn a_push_another_process_asks_is_answered_by_the_nodes_relay() {
    let lab = Lab::new();
    let mut macs: Vec<(Mac, Arc<Fake>)> =
        (1..=2).map(|tag| tables_mac(&lab, tag, &["w"])).collect();
    start_all(&lab, &mut macs);
    macs[0].1.write("w", "pushed");
    let state = macs[0].0.mesh().state_dir().to_path_buf();
    assert_eq!(
        crate::push::push_by_node(&state, &TABLE, Instant::now() + HANG),
        Some(crate::Pushed::Reached)
    );
    macs[1].1.until_holds("pushed");
    let unserved = aterm_tempfile::TempDir::new().expect("a scratch folder");
    std::fs::create_dir_all(unserved.path().join(crate::logs::MESH_DIR)).expect("its mesh folder");
    assert_eq!(
        crate::push::push_by_node(unserved.path(), &TABLE, Instant::now() + HANG),
        None,
        "no node answers"
    );
}

/// `a`'s link to `b`.
fn link_between(a: &Mac, b: &Mac) -> Arc<aterm_net::fleet::link::Link> {
    a.run
        .as_ref()
        .expect("A runs")
        .table
        .links()
        .into_iter()
        .find(|link| astream_broker::identity_principal(link.peer().key.bytes()) == b.principal)
        .expect("a link to B")
}

/// Cut `b` off, and wait until `a`'s relays to it ended.
fn cut_off(lab: &Lab, a: &Mac, b: &Mac) {
    let since = a.seen.mark();
    b.cut(lab);
    a.seen.until("A's relays to B end", |events| {
        let ended: BTreeSet<MeshLog> = events
            .iter()
            .skip(since)
            .filter_map(|event| match event {
                Event::Ended { peer, log, .. } if *peer == b.principal => Some(*log),
                _ => None,
            })
            .collect();
        ended.len() == MeshLog::ALL.len()
    });
}

/// Whether `a` ran an `Inventory` with `b` for `NewLink` from event `since` on: an exchange that
/// opened a link, not one skipped as quiet.
fn ran_new_link(events: &[Event], b: &Mac, since: usize) -> bool {
    events.iter().skip(since).any(|event| {
        matches!(
            event,
            Event::Inventory {
                peer,
                reason: Some(Reason::NewLink),
                ..
            } if *peer == b.principal
        )
    })
}

/// Heal `b`'s cut and wait until `a`'s relays follow it again and `a` ran its `NewLink` exchange
/// with `b` there. The relays resume with their cursors and ask nothing, so the exchange is the
/// one asked before the cut.
fn heal_until_ran(lab: &Lab, a: &Mac, b: &Mac) {
    let since = a.seen.mark();
    b.heal(lab);
    a.until_following(&[b], since);
    a.seen.until("the ask runs over the next link", |events| {
        ran_new_link(events, b, since)
    });
}

/// An ask outlives the link it was asked over: one whose link closed before the worker ran it (a
/// duplicate that replaced a new link, before its start's `NewLink` ask ran) runs over the
/// neighbour's next link, though the relay that resumes there with its cursor asks nothing itself.
#[test]
fn an_ask_whose_link_closed_runs_over_the_next() {
    let lab = Lab::new();
    let mut macs: Vec<(Mac, Arc<Fake>)> =
        (1..=2).map(|tag| tables_mac(&lab, tag, &["w"])).collect();
    start_all(&lab, &mut macs);
    let (a, b) = (&macs[0].0, &macs[1].0);
    let link = link_between(a, b);
    cut_off(&lab, a, b);
    // A's summary moves past the one its first exchange left quiet, so the ask opens a link.
    macs[0].1.write("w", "while-cut");
    let inventory = a.mesh().inventory().expect("an Inventory");
    inventory.request(&b.principal, &link, Reason::NewLink);
    assert!(inventory.wait_idle_for_test(Instant::now() + HANG));
    heal_until_ran(&lab, a, b);
}

/// An exchange that broke off on its link runs again over the neighbour's next link: B's store
/// fails the exchange A asks over their live link, and once it serves again, A's ask runs over
/// the link the heal of a cut opens, where the relays resume with their cursors and ask nothing.
#[test]
fn an_exchange_that_broke_off_runs_again_over_the_next_link() {
    let lab = Lab::new();
    let mut macs: Vec<(Mac, Arc<Fake>)> =
        (1..=2).map(|tag| tables_mac(&lab, tag, &["w"])).collect();
    start_all(&lab, &mut macs);
    let (a, b) = (&macs[0].0, &macs[1].0);
    let link = link_between(a, b);
    macs[1].1.refuse(true);
    // A's summary moves past the one its first exchange left quiet, so the ask opens the link.
    macs[0].1.write("w", "unanswered");
    let inventory = a.mesh().inventory().expect("an Inventory");
    let since = a.seen.mark();
    inventory.request(&b.principal, &link, Reason::NewLink);
    assert!(inventory.wait_idle_for_test(Instant::now() + HANG));
    assert!(
        !ran_new_link(&lock(&a.seen.events), b, since),
        "the exchange broke off"
    );
    macs[1].1.refuse(false);
    cut_off(&lab, a, b);
    heal_until_ran(&lab, a, b);
}

/// `MeshRelay`'s conformance (§11 B8), on this lab's Macs and stand-in stores.
#[path = "relay_conformance_tests.rs"]
mod relay_conformance;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vault's head: an open through it holds what a full open of the same file holds and the
//! resolve reads the same entries from it, it reads no replaced body, it serves a peer every
//! replaced body as the file holds it, and any head that does not name the file as it is, under
//! this Mac's identity and the reader's context, is not read.

use super::*;
use crate::SecretBytes;
use crate::age::Identity;
use crate::membership_log::entries::{inner, resolves_and_opens};
#[cfg(unix)]
use crate::membership_log::read_records;
use crate::membership_log::tests::signed::{Mac, founded, generated, three_macs};
use crate::membership_log::{EpochAudience, Log, Store, encode_records};
use crate::text::{Audience, Entry};
#[cfg(unix)]
use std::io::Read as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

/// A records file and its head's place in a scratch folder.
struct Vault {
    _dir: aterm_tempfile::TempDir,
    data: PathBuf,
    head: PathBuf,
}

impl Vault {
    /// A folder whose records file holds `log`'s records, in its order.
    fn of(log: &Log) -> Self {
        let dir = aterm_tempfile::tempdir().unwrap();
        let data = dir.path().join("records");
        let head = crate::records_storage::head_path(&data).unwrap();
        fsx::write_atomic(&data, &encode_records(log.records()), 0o600).unwrap();
        Self {
            _dir: dir,
            data,
            head,
        }
    }

    fn empty() -> Store {
        Store::for_vault("").unwrap()
    }

    fn context() -> [u8; 32] {
        context(&Self::empty(), "")
    }

    /// The file read in full, as the records cache reads one, marked as the file it is, whose
    /// head is written for `owner`, by a read that began after the file settled.
    fn full_for(&self, owner: &Identity) -> Store {
        self.full_for_at(owner, std::time::SystemTime::now() + 2 * fsx::SETTLED)
    }

    /// [`Vault::full_for`] by a read that began at `began`.
    fn full_for_at(&self, owner: &Identity, began: std::time::SystemTime) -> Store {
        let bytes = std::fs::read(&self.data).unwrap();
        let mut store = Self::empty().recovering(&bytes);
        let stamp = fsx::stamp(&self.data).unwrap();
        store.read_from(Origin::new(
            &self.head,
            stamp,
            digest(&bytes),
            Self::context(),
            owner.recipient().0,
            began,
        ));
        store
    }

    /// The file read in full, as a reader with no identity reads it: no head is written.
    fn full(&self) -> Store {
        Self::empty().recovering(&std::fs::read(&self.data).unwrap())
    }

    /// The file read through its head under `identity`, where the head names it.
    fn through_head(&self, identity: &Identity) -> Option<Store> {
        self.through_head_in(identity, Self::context())
    }

    fn through_head_in(&self, identity: &Identity, context: [u8; 32]) -> Option<Store> {
        let file = std::fs::File::open(&self.data).unwrap();
        let stamp = fsx::FileStamp::of(&fsx::meta_of(&file).unwrap());
        open(
            &self.head,
            &file,
            stamp,
            context,
            identity,
            Self::empty(),
            Publication::Allowed,
        )
        .map(|(store, _)| store)
    }

    /// The head written by a full open's resolve under `identity`; whether one was written.
    fn written_by(&self, identity: &Identity) -> bool {
        let before = WRITES.get();
        let store = self.full_for(identity);
        let _ = store
            .log()
            .unwrap()
            .open_audience_entries(identity)
            .unwrap();
        WRITES.get() == before + 1
    }
}

/// What two resolves of one identity read, compared whole: every value, every register's
/// evidence and every epoch verdict ([`OpenedAudienceEntries`]'s fields, through its `Debug`,
/// which shows no value, and its entries).
fn same_reading(full: &Log, read: &Log, identity: &Identity, what: &str) {
    let want = full.open_audience_entries(identity).unwrap();
    let have = read.open_audience_entries(identity).unwrap();
    assert_eq!(
        want.as_ref().map(|o| &o.by_audience),
        have.as_ref().map(|o| &o.by_audience),
        "{what}: entries"
    );
    assert_eq!(
        want.as_ref().map(|o| &o.commons_control),
        have.as_ref().map(|o| &o.commons_control),
        "{what}: Commons control"
    );
    assert_eq!(format!("{want:?}"), format!("{have:?}"), "{what}: evidence");
    assert_eq!(
        full.open_entry_set(identity).unwrap(),
        read.open_entry_set(identity).unwrap(),
        "{what}: merged entries"
    );
}

/// What two stores hold, compared: their records in order (what a history's generation digests),
/// heads, membership, budgets' counts and bytes, and what a pull names.
fn same_store(full: &Store, read: &Store, what: &str) {
    let (f, r) = (full.log().unwrap(), read.log().unwrap());
    let ids = |log: &Log| log.records().map(|r| *r.id()).collect::<Vec<_>>();
    assert_eq!(ids(f), ids(r), "{what}: records in order (the generation)");
    assert_eq!(f.heads(), r.heads(), "{what}: heads");
    assert_eq!(f.membership(), r.membership(), "{what}: membership");
    assert_eq!(f.counts(), r.counts(), "{what}: counts");
    assert_eq!(full.encoded_len(), read.encoded_len(), "{what}: bytes");
    assert_eq!(full.frontier(), read.frontier(), "{what}: frontier");
    assert_eq!(full.have(), read.have(), "{what}: have");
}

fn shelved(store: &Store) -> usize {
    store
        .log()
        .unwrap()
        .records()
        .filter(|record| record.is_shelved())
        .count()
}

/// `log`'s causal past of the record halfway along it: a log a later one extends.
fn halfway(log: &Log) -> Log {
    let mid = log.records().nth(log.held_count() / 2).unwrap();
    log.project_heads(&[*mid.id()]).unwrap()
}

#[test]
fn vault_head_open_and_full_open_agree_on_entries_membership_heads_and_generation() {
    let (macs, _) = three_macs();
    let mut shelved_in_all = 0;
    let mut extended = 0;
    for seed in 0..16 {
        let log = generated(seed);
        for mac in &macs {
            let what = format!(
                "seed {seed}, m{}",
                macs.iter().position(|m| m.member == mac.member).unwrap() + 1
            );
            let vault = Vault::of(&log);
            if !vault.written_by(&mac.identity) {
                // A Mac the log removed resolves nothing, and writes no head.
                assert!(
                    log.open_audience_entries(&mac.identity).unwrap().is_none(),
                    "{what}"
                );
                assert!(vault.through_head(&mac.identity).is_none(), "{what}");
                continue;
            }
            let full = vault.full();
            let read = vault
                .through_head(&mac.identity)
                .expect("the head names the file");
            shelved_in_all += shelved(&read);
            same_store(&full, &read, &what);
            for reader in &macs {
                // Another identity reads the shelved records back as a full resolve opens them.
                same_reading(
                    full.log().unwrap(),
                    read.log().unwrap(),
                    &reader.identity,
                    &what,
                );
            }

            // Records that arrive after the head: the resolve opens what they change and takes
            // the rest from the head.
            let early = halfway(&log);
            let vault = Vault::of(&early);
            if !vault.written_by(&mac.identity) {
                continue;
            }
            let mut read = vault.through_head(&mac.identity).unwrap();
            let mut full = Vault::empty().recovering(&std::fs::read(&vault.data).unwrap());
            for record in log.records() {
                let _ = read.insert(record.clone());
                let _ = full.insert(record.clone());
            }
            same_store(&full, &read, &format!("{what}, extended"));
            same_reading(
                full.log().unwrap(),
                read.log().unwrap(),
                &mac.identity,
                &format!("{what}, extended"),
            );
            extended += 1;
        }
    }
    assert!(
        shelved_in_all > 0,
        "the generated logs shelve replaced bodies"
    );
    assert!(
        extended > 0,
        "a head is read under records that arrive after it"
    );
}

/// A resolve of the head's entries whose register the head mislabels reads otherwise: the
/// negative control of the differential above, which shows it reads the head's classes.
#[test]
fn vault_head_differential_catches_a_head_that_misclasses_a_replaced_put() {
    let (macs, _) = three_macs();
    let caught = (0..16).any(|seed| {
        let log = generated(seed);
        let mac = &macs[0];
        let vault = Vault::of(&log);
        if !vault.written_by(&mac.identity) {
            return false;
        }
        let full = vault.full();
        let mut read = vault.through_head(&mac.identity).unwrap();
        let read_log = read.log.as_mut().unwrap();
        let ops = read_log.head.as_deref().unwrap();
        // MUTANT: every shelved op writes what its entry's winner writes.
        let mut wrong = BTreeMap::new();
        for (id, op) in &ops.ops {
            let shelved = read_log.get(id).is_some_and(Record::is_shelved);
            wrong.insert(
                *id,
                Op {
                    class: if shelved { 0 } else { op.class },
                    ..*op
                },
            );
        }
        read_log.head = Some(Arc::new(Ops {
            recipient: ops.recipient,
            ops: wrong,
        }));
        let want = full
            .log()
            .unwrap()
            .open_audience_entries(&mac.identity)
            .unwrap();
        let have = read
            .log()
            .unwrap()
            .open_audience_entries(&mac.identity)
            .unwrap();
        format!("{want:?}") != format!("{have:?}")
    });
    assert!(
        caught,
        "a misclassed replaced Put changes some generated log's reading"
    );
}

#[test]
fn vault_head_torn_or_altered_falls_back_to_full_verification() {
    let (log, macs, epoch, epoch_key) = inventory_log(4, 1024, 2);
    let mac = &macs[0];
    let vault = Vault::of(&log);
    assert!(vault.written_by(&mac.identity));
    assert!(
        vault.through_head(&mac.identity).is_some(),
        "control: the head as written is read"
    );
    let head = std::fs::read(&vault.head).unwrap();

    // Each byte of the head altered, and the head cut at each length: none is read.
    for at in (0..head.len())
        .step_by(head.len() / 97 + 1)
        .chain([head.len() - 1])
    {
        let mut altered = head.clone();
        altered[at] ^= 0x01;
        fsx::write_atomic(&vault.head, &altered, 0o600).unwrap();
        assert!(
            vault.through_head(&mac.identity).is_none(),
            "byte {at} altered"
        );
        fsx::write_atomic(&vault.head, &head[..at], 0o600).unwrap();
        assert!(vault.through_head(&mac.identity).is_none(), "cut at {at}");
    }
    fsx::write_atomic(&vault.head, &head, 0o600).unwrap();
    assert!(
        vault.through_head(&mac.identity).is_some(),
        "control: put back, it is read"
    );

    // Another identity, another context: not read.
    assert!(
        vault.through_head(&macs[1].identity).is_none(),
        "another Mac's identity"
    );
    assert!(
        vault
            .through_head_in(&mac.identity, digest(b"another vault"))
            .is_none(),
        "another context"
    );
    // A head another identity wrote and sealed in this one's name: its MAC is not this Mac's.
    let mut forged = head.clone();
    let signed = forged.len() - 32;
    let tag = ring::hmac::sign(&key(&macs[1].identity), &forged[..signed]);
    forged[signed..].copy_from_slice(tag.as_ref());
    fsx::write_atomic(&vault.head, &forged, 0o600).unwrap();
    assert!(
        vault.through_head(&mac.identity).is_none(),
        "sealed under another identity"
    );
    fsx::write_atomic(&vault.head, &head, 0o600).unwrap();

    // The file a writer appended to (an older build, a concurrent writer): stale.
    let original = std::fs::read(&vault.data).unwrap();
    let mut grown = log.clone();
    let entry = Entry {
        name: "late".to_owned(),
        value: SecretBytes::from(b"v".as_slice()),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    let draft = grown
        .draft_put(&mac.member, 9_000, epoch, &epoch_key, &entry)
        .unwrap();
    mac.sign(&mut grown, draft);
    let appended = grown.records().last().unwrap().clone();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&vault.data)
        .unwrap();
    std::io::Write::write_all(&mut file, appended.bytes()).unwrap();
    drop(file);
    assert!(
        vault.through_head(&mac.identity).is_none(),
        "appended since"
    );
    // The full open takes the appended record, and its resolve writes the head again.
    let full = vault.full();
    assert!(full.holds(appended.id()));
    assert!(vault.written_by(&mac.identity), "rebuilt");
    let read = vault
        .through_head(&mac.identity)
        .expect("the rebuilt head is read");
    assert!(read.holds(appended.id()));

    // The file replaced whole by one of the same length (a rename over it): its stamp moved, and
    // the head is read only where the file's digest is the one it names.
    let replaced = std::fs::read(&vault.data).unwrap();
    fsx::write_atomic(&vault.data, &replaced, 0o600).unwrap();
    assert!(
        vault.through_head(&mac.identity).is_some(),
        "the same bytes under another stamp: the digest names them"
    );
    let mut other = replaced.clone();
    let last = other.len() - 1;
    other[last] ^= 0x01;
    fsx::write_atomic(&vault.data, &other, 0o600).unwrap();
    assert!(
        vault.through_head(&mac.identity).is_none(),
        "another file of that length"
    );
    fsx::write_atomic(&vault.data, &original, 0o600).unwrap();
    assert!(
        vault.through_head(&mac.identity).is_none(),
        "the file before the append"
    );
}

/// A Mac's log as the measured one is shaped (`docs/DESIGN-vault-log-2026-11-14.md` §1): one
/// large entry rewritten `copies` times, beside `small` entries of 2 KB and the membership.
fn inventory_log(
    copies: u32,
    inventory: usize,
    small: u32,
) -> (Log, Vec<Mac>, RecordId, zeroize::Zeroizing<[u8; 32]>) {
    let (macs, entrants) = three_macs();
    let (mut log, epoch, key) = founded(&macs, &entrants);
    for i in 0..copies {
        let entry = Entry {
            name: "inventory".to_owned(),
            value: SecretBytes::from(
                vec![b'a' + u8::try_from(i % 26).unwrap(); inventory].as_slice(),
            ),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let draft = log
            .draft_put(&macs[0].member, 2_000 + u64::from(i), epoch, &key, &entry)
            .unwrap();
        macs[0].sign(&mut log, draft);
    }
    for i in 0..small {
        let entry = Entry {
            name: format!("token-{i}"),
            value: SecretBytes::from(
                [&i.to_be_bytes()[..], &[b'x'; 2_000][..]]
                    .concat()
                    .as_slice(),
            ),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let draft = log
            .draft_put(&macs[1].member, 3_000 + u64::from(i), epoch, &key, &entry)
            .unwrap();
        macs[1].sign(&mut log, draft);
    }
    (log, macs, epoch, key)
}

#[test]
fn vault_head_open_reads_no_replaced_body() {
    let (log, macs, _, _) = inventory_log(24, 64 * 1024, 8);
    let identity = &macs[0].identity;
    let vault = Vault::of(&log);
    let file_len = std::fs::metadata(&vault.data).unwrap().len() as usize;
    assert!(vault.written_by(identity));

    let read = vault.through_head(identity).unwrap();
    let source = read.shelf_source().expect("replaced bodies are shelved");
    assert_eq!(
        shelved(&read),
        23,
        "every replaced copy of the inventory is shelved"
    );
    let (_, opens_before) = crate::membership_log::entries::resolves_and_opens();
    let opened = read
        .log()
        .unwrap()
        .open_entry_set(identity)
        .unwrap()
        .unwrap();
    let (_, opens_after) = crate::membership_log::entries::resolves_and_opens();
    assert_eq!(opened.entries.len(), 9);
    assert_eq!(
        source.read_bytes(),
        0,
        "neither the open nor the resolve read a replaced body back"
    );
    assert_eq!(shelved(&read), 23, "and none is held in memory");
    // The resolve opens the live ops (the inventory's winner and the 8 small entries), each
    // once to read it and once to take its value, and no replaced one.
    assert_eq!(opens_after - opens_before, 2 * 9);
    let live: usize = read
        .log()
        .unwrap()
        .records()
        .filter(|record| !record.is_shelved())
        .map(Record::wire_len)
        .sum();
    assert!(
        read.footprint() < file_len / 8,
        "{} of {file_len} bytes kept",
        read.footprint()
    );
    assert!(live < file_len / 8);
    assert_eq!(
        read.shelf_len(),
        file_len as u64,
        "the records file it holds open, which the records cache charges"
    );
    assert_eq!(vault.full().shelf_len(), 0);

    // NEGATIVE CONTROL: the same open of the full file opens every copy.
    let full = vault.full();
    let (_, opens_before) = crate::membership_log::entries::resolves_and_opens();
    let _ = full.log().unwrap().open_entry_set(identity).unwrap();
    let (_, opens_after) = crate::membership_log::entries::resolves_and_opens();
    assert_eq!(opens_after - opens_before, 24 + 8 + 9);
    assert!(full.footprint() >= file_len);
}

#[cfg(unix)]
#[test]
fn vault_head_serves_a_peer_the_replaced_bodies_byte_for_byte() {
    let (log, macs, _, _) = inventory_log(12, 16 * 1024, 4);
    let identity = &macs[0].identity;
    let vault = Vault::of(&log);
    assert!(vault.written_by(identity));
    let file = std::fs::read(&vault.data).unwrap();

    let read = vault.through_head(identity).unwrap();
    let source = read.shelf_source().unwrap();
    // What a peer that holds nothing is sent: every record, a replaced one read back from the
    // file and checked against its id, by the server's reply and by a Mac's post, neither of
    // which keeps a body it read back.
    let missing = read.missing(&[]);
    let mut sent = Vec::new();
    crate::server::record_chunks(&missing, |chunk| {
        sent.extend_from_slice(chunk);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        sent, file,
        "the stream a peer is sent is the file, byte for byte"
    );
    let mut posted = Vec::new();
    let mut body = crate::transport::records_body::RecordsReader::new(&missing).unwrap();
    assert_eq!(body.len(), file.len());
    body.read_to_end(&mut posted).unwrap();
    assert_eq!(posted, file, "a post's body is the file, byte for byte");
    assert_eq!(read.read_encoded().unwrap(), file);
    // What aterm-db's Commons hello and witness send (every record of the log), and what its
    // batches and the re-key bound count, which reads nothing.
    assert_eq!(read_records(read.log().unwrap().records()).unwrap(), file);
    assert_eq!(
        read.log()
            .unwrap()
            .records()
            .map(Record::wire_len)
            .sum::<usize>(),
        file.len()
    );
    assert!(source.read_bytes() > 0);
    assert_eq!(
        shelved(&read),
        11,
        "sending keeps none of what it read back"
    );
    let mut peer = Vault::empty();
    assert_eq!(peer.ingest(&sent).unwrap(), log.held_count());
    // A write of the file whole from a store read through the head writes the same bytes, and
    // keeps no replaced body it read for it.
    let again = vault.through_head(identity).unwrap();
    let mut written = Vec::new();
    again.write_encoded(&mut written).unwrap();
    assert_eq!(written, file);
    assert_eq!(
        shelved(&again),
        11,
        "a whole write keeps none of what it read back"
    );

    // NEGATIVE CONTROL: a replaced body changed in place under an open store is not served or
    // written: its bytes no longer hash to its id.
    let tampered = vault.through_head(identity).unwrap();
    let mut bytes = file.clone();
    let record = tampered
        .log()
        .unwrap()
        .records()
        .find(|record| record.is_shelved())
        .unwrap()
        .clone();
    let at: usize = log
        .records()
        .take_while(|other| other.id() != record.id())
        .map(Record::wire_len)
        .sum::<usize>()
        + record.wire_len() / 2;
    bytes[at] ^= 0x01;
    let file_handle = std::fs::OpenOptions::new()
        .write(true)
        .open(&vault.data)
        .unwrap();
    std::os::unix::fs::FileExt::write_all_at(&file_handle, &bytes[at..=at], at as u64).unwrap();
    drop(file_handle);
    assert!(
        record.wire_bytes().is_err(),
        "no bytes but the ones its id names"
    );
    assert!(
        tampered.write_encoded(&mut Vec::new()).is_err(),
        "no whole write past it"
    );
}

/// A shelved body that does not read back as it was (the file changed under the open store, a
/// read that fails) fails what sends or copies it, never leaving the record out of a stream a
/// peer would take short: the server's reply, a Mac's post, a bundle's records, a witness.
#[cfg(unix)]
#[test]
fn vault_head_body_that_does_not_read_back_is_an_error_not_a_short_stream() {
    let (log, macs, _, _) = inventory_log(6, 16 * 1024, 2);
    let identity = &macs[0].identity;
    let vault = Vault::of(&log);
    assert!(vault.written_by(identity));
    let read = vault.through_head(identity).unwrap();
    let missing = read.missing(&[]);
    assert!(
        read_records(&missing).is_ok(),
        "control: as written, it reads back"
    );

    // The file's last record is the newest small entry's, kept whole; flip a byte of the first
    // shelved body in place.
    let record = missing.iter().find(|record| record.is_shelved()).unwrap();
    let at: u64 = missing
        .iter()
        .take_while(|other| other.id() != record.id())
        .map(|other| other.wire_len() as u64)
        .sum::<u64>()
        + record.wire_len() as u64 / 2;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&vault.data)
        .unwrap();
    let mut byte = [0u8; 1];
    std::os::unix::fs::FileExt::read_exact_at(&file, &mut byte, at).unwrap();
    byte[0] ^= 0x01;
    std::os::unix::fs::FileExt::write_all_at(&file, &byte, at).unwrap();
    drop(file);

    assert!(record.wire_bytes().is_err());
    assert!(read_records(&missing).is_err(), "a witness of the log");
    assert!(read.read_encoded().is_err(), "a post of the whole log");
    assert!(read.read_held().is_err(), "a bundle's records");
    assert!(
        crate::server::record_chunks(&missing, |_| Ok(())).is_err(),
        "the server's reply"
    );
    let mut body = crate::transport::records_body::RecordsReader::new(&missing).unwrap();
    assert!(body.read_to_end(&mut Vec::new()).is_err(), "a Mac's post");
}

/// A head is read only under the rules it was written under: those of the build's sources
/// (`build.rs`'s `ATERM_SECRETS_RULES`), not the crate's version, which every build shares.
#[test]
fn vault_head_is_read_only_under_the_rules_it_was_written_under() {
    assert_ne!(RULES, env!("CARGO_PKG_VERSION"));
    assert_eq!(RULES.len(), 32, "a digest of the build's sources");
    let (log, macs, _, _) = inventory_log(4, 1024, 2);
    let identity = &macs[0].identity;
    let vault = Vault::of(&log);
    assert!(vault.written_by(identity));
    let ours = context_under(RULES, &Vault::empty(), "");
    assert_eq!(
        ours,
        Vault::context(),
        "a store's context is this build's rules'"
    );
    assert!(
        vault.through_head_in(identity, ours).is_some(),
        "control: the rules it was written under"
    );
    let other = context_under("another build's sources", &Vault::empty(), "");
    assert_ne!(other, ours);
    assert!(
        vault.through_head_in(identity, other).is_none(),
        "a build of other rules reads no head this one wrote"
    );
}

/// An open of a head written for another identity, under another context, or naming a records
/// file of another length (the first open after an append) reads the head's fixed fields alone
/// and stops: each sends it to the full open, so neither the rest of the head nor its MAC is read.
#[test]
fn vault_head_open_checks_its_fixed_fields_before_reading_the_rest() {
    let (log, macs, _, _) = inventory_log(12, 16 * 1024, 4);
    let vault = Vault::of(&log);
    assert!(vault.written_by(&macs[0].identity));
    let before = MACS.get();
    assert!(
        vault.through_head(&macs[0].identity).is_some(),
        "control: the head as written is read"
    );
    assert_eq!(MACS.get(), before + 1, "control: its MAC is checked");

    assert!(vault.through_head(&macs[1].identity).is_none());
    let other = context_under("another build's sources", &Vault::empty(), "");
    assert!(vault.through_head_in(&macs[0].identity, other).is_none());
    let mut appended = std::fs::read(&vault.data).unwrap();
    appended.extend_from_slice(&[0; 64]);
    fsx::write_atomic(&vault.data, &appended, 0o600).unwrap();
    assert!(vault.through_head(&macs[0].identity).is_none());
    assert_eq!(
        MACS.get(),
        before + 1,
        "another identity, context or length checks no MAC"
    );
}

/// What [`RULES`] digests (`build.rs` names each input in `rules_inputs.txt`): every module a
/// build without tests compiles, ring's locked release, whose Ed25519 verifier decides what a
/// record's signature is, and no module only a test build reads, whatever its file is named, so a
/// test-only edit leaves every unit fresh.
#[test]
fn vault_head_rules_cover_what_a_build_reads_and_no_test_module() {
    let inputs: Vec<&str> = include_str!(concat!(env!("OUT_DIR"), "/rules_inputs.txt"))
        .lines()
        .collect();
    for read in [
        "aterm-secrets:lib.rs",
        "aterm-secrets:membership_log/head.rs",
        "aterm-secrets:membership_log/store.rs",
        "aterm-secrets:membership_log/entries/register.rs",
        "aterm-secrets:x25519.rs",
        "astream-aead:signature.rs",
    ] {
        assert!(inputs.contains(&read), "{read} is among the rules");
    }
    assert!(
        inputs
            .iter()
            .any(|input| input.starts_with("Cargo.lock:ring ")),
        "ring's release is among the rules: {inputs:?}"
    );
    for test_only in [
        "aterm-secrets:membership_log/entries/register/oracle.rs",
        "aterm-secrets:membership_log/entries/keyed/oracle.rs",
        "aterm-secrets:membership_log/epochs/oracle.rs",
        "aterm-secrets:membership_log/eval/full_pass_oracle.rs",
        "aterm-secrets:membership_log/untaken/chain.rs",
        "aterm-secrets:x25519/tweetnacl.rs",
        "aterm-secrets:membership_log/store/whole_tests.rs",
        "aterm-secrets:membership_log/head_tests.rs",
    ] {
        assert!(!inputs.contains(&test_only), "{test_only} is a test module");
    }
}

/// The `Put`s of `value` to the Claude Code token entry `name` under the `[everyone]` epoch.
fn token(name: &str, value: &[u8]) -> Entry {
    Entry {
        name: name.to_owned(),
        value: SecretBytes::from(value),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    }
}

const TOKEN: &str = "aterm-claude-token-ac-0123456789abcdef";

/// A Claude Code token entry's writes read the same or not by the epoch keys the reading log
/// holds: a seal of an epoch this Mac gains later, carried as it is by another Mac's write,
/// reads as the value it seals once that `Wrap` is held, and as its sealed bytes before. The
/// head leaves such an entry out, so a log read through it and then given that `Wrap`, or
/// projected to before it, reads the entry as a full open of the same records does.
#[test]
fn vault_head_leaves_out_an_entry_whose_reading_the_epoch_keys_change() {
    let (macs, entrants) = three_macs();
    let (mut log, epoch, key) = founded(&macs, &entrants);
    let (first, second, reader) = (&macs[0], &macs[1], &macs[2]);
    // An epoch only the first Mac holds yet.
    let (draft, hidden_key) = log
        .draft_epoch(&first.member, 1_500, EpochAudience::Commons, 2)
        .unwrap();
    let hidden = first.sign(&mut log, draft);
    let wrap = log
        .draft_wrap(
            &first.member,
            1_501,
            hidden,
            &hidden_key,
            first.identity.recipient(),
        )
        .unwrap();
    first.sign(&mut log, wrap);
    for (ms, value) in [(2_000, b"token-0"), (2_001, b"token-1")] {
        let draft = log
            .draft_put(&first.member, ms, epoch, &key, &token(TOKEN, value))
            .unwrap();
        first.sign(&mut log, draft);
    }
    // Concurrent writes of one value: the first Mac's sealed under the `[everyone]` epoch, the
    // second's carrying the first's seal of it under the hidden epoch as it is.
    let carried = inner::store(log.network(), &hidden, &hidden_key, TOKEN, b"token-2").unwrap();
    let mut beside = log.clone();
    let draft = log
        .draft_put(&first.member, 2_002, epoch, &key, &token(TOKEN, b"token-2"))
        .unwrap();
    let edit = first.sign(&mut log, draft);
    let draft = beside
        .draft_put(&second.member, 2_002, epoch, &key, &token(TOKEN, &carried))
        .unwrap();
    let carry = second.sign(&mut beside, draft);
    log.insert(beside.get(&carry).unwrap().clone()).unwrap();
    // The hidden epoch handed to the reader, after both.
    let mut handed = log.clone();
    let draft = handed
        .draft_wrap(
            &first.member,
            3_000,
            hidden,
            &hidden_key,
            reader.identity.recipient(),
        )
        .unwrap();
    let handover = first.sign(&mut handed, draft);
    let handover = handed.get(&handover).unwrap().clone();

    // NEGATIVE CONTROL: the reader reads the entry otherwise once it holds the hidden epoch.
    let reading = |log: &Log| format!("{:?}", log.open_audience_entries(&reader.identity).unwrap());
    assert_ne!(
        reading(&log),
        reading(&handed),
        "the keys held change the reading"
    );

    // Read through a head written without the hidden epoch, then handed it.
    let vault = Vault::of(&log);
    assert!(vault.written_by(&reader.identity));
    let mut read = vault.through_head(&reader.identity).unwrap();
    let mut full = vault.full();
    read.insert(handover.clone()).unwrap();
    full.insert(handover).unwrap();
    same_reading(
        full.log().unwrap(),
        read.log().unwrap(),
        &reader.identity,
        "handed the epoch after the head",
    );

    // Read through a head written with it, then projected to before it.
    let vault = Vault::of(&handed);
    assert!(vault.written_by(&reader.identity));
    let read = vault.through_head(&reader.identity).unwrap();
    let full = vault.full();
    let frontier = [edit, carry];
    same_reading(
        &full.log().unwrap().project_heads(&frontier).unwrap(),
        &read.log().unwrap().project_heads(&frontier).unwrap(),
        &reader.identity,
        "projected to before the epoch",
    );
}

/// The lineage of a Claude Code token (each `Put` of its value) reads no replaced body of
/// another entry, and a replaced body read back (a peer asked for it) leaves what the head says
/// of it standing: the next resolve opens no more than one of a store that read none back.
#[test]
fn vault_head_token_lineage_and_a_body_read_back_leave_replaced_bodies_unopened() {
    let (mut log, macs, epoch, key) = inventory_log(12, 16 * 1024, 2);
    let identity = &macs[0].identity;
    for (ms, value) in [(4_000, b"token-a"), (4_001, b"token-b")] {
        let draft = log
            .draft_put(&macs[0].member, ms, epoch, &key, &token(TOKEN, value))
            .unwrap();
        macs[0].sign(&mut log, draft);
    }
    let vault = Vault::of(&log);
    assert!(vault.written_by(identity));
    let read = vault.through_head(identity).unwrap();
    let source = read.shelf_source().unwrap();

    let lineage = |store: &Store, value: &[u8]| {
        store
            .log()
            .unwrap()
            .put_lineage(identity, TOKEN, value)
            .unwrap()
            .unwrap()
    };
    let full = vault.full();
    for value in [b"token-a".as_slice(), b"token-b"] {
        assert_eq!(lineage(&read, value), lineage(&full, value));
        assert_eq!(lineage(&read, value).puts.len(), 1);
    }
    // What each draft put under the token's name, as `claude_token::settle` reads a split pair.
    let drafts: Vec<_> = log
        .records()
        .filter_map(|record| match record.body() {
            crate::membership_log::Body::Put { epoch, .. } => {
                Some((*record.actor(), record.authored_ms(), *epoch))
            }
            _ => None,
        })
        .collect();
    let of_drafts = |store: &Store| {
        let log = store.log().unwrap();
        let opened = log.open_audience_entries(identity).unwrap().unwrap();
        log.everyone_puts_of_drafts(&opened, TOKEN, &drafts)
            .unwrap()
            .into_iter()
            .map(|put| match put {
                crate::membership_log::entries::DraftPut::Wrote(value) => {
                    format!("wrote {:?}", value.as_slice())
                }
                crate::membership_log::entries::DraftPut::Unopened => "unopened".to_owned(),
                crate::membership_log::entries::DraftPut::Nothing => "nothing".to_owned(),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(of_drafts(&read), of_drafts(&full));
    assert_eq!(
        source.read_bytes(),
        0,
        "a token's lineage and its drafts' writes read no replaced body"
    );

    let opens = |store: &Store| {
        let (_, before) = resolves_and_opens();
        let _ = store.log().unwrap().open_entry_set(identity).unwrap();
        resolves_and_opens().1 - before
    };
    let unread = opens(&vault.through_head(identity).unwrap());
    let kept = read.footprint();
    for record in read.log().unwrap().records() {
        if record.is_shelved() {
            assert!(!record.bytes().is_empty(), "read back and kept");
        }
    }
    assert_eq!(shelved(&read), 0);
    assert!(
        read.footprint() > kept + 11 * 16 * 1024,
        "what was read back is counted as kept"
    );
    assert_eq!(
        opens(&read),
        unread,
        "a body read back is still taken as the head says"
    );
}

/// The file's bytes under a moved stamp (renamed over with the same bytes): the open hashes the
/// file against the head's digest once and writes the head again under the new stamp, so the
/// next open reads by the stamp alone.
#[test]
fn vault_head_names_a_moved_stamp_so_the_file_is_hashed_once() {
    let (log, macs, _, _) = inventory_log(4, 1024, 2);
    let identity = &macs[0].identity;
    let vault = Vault::of(&log);
    assert!(vault.written_by(identity));
    let hashed = |open: &dyn Fn() -> Option<Store>| {
        let before = HASHES.get();
        assert!(open().is_some());
        HASHES.get() - before
    };
    let open = || vault.through_head(identity);
    let settled = || fsx::as_if_later(2 * fsx::SETTLED, || vault.through_head(identity));
    assert_eq!(hashed(&open), 0, "control: the stamp the head names");
    let bytes = std::fs::read(&vault.data).unwrap();
    fsx::write_atomic(&vault.data, &bytes, 0o600).unwrap();
    // A stamp within its file's own tick may yet show other bytes: hashed, not named.
    assert_eq!(hashed(&open), 1, "a moved stamp: hashed against the digest");
    assert_eq!(hashed(&open), 1, "an unsettled stamp is not named");
    assert_eq!(hashed(&settled), 1, "settled: hashed once more");
    assert_eq!(hashed(&open), 0, "the head names the new stamp");
    // NEGATIVE CONTROL: another file of that length under a moved stamp is still refused.
    let mut other = bytes.clone();
    let last = other.len() - 1;
    other[last] ^= 0x01;
    fsx::write_atomic(&vault.data, &other, 0o600).unwrap();
    assert!(open().is_none());
}

/// The resolve after a full open writes the head from what that read found, without reading the
/// records file again, and tries once: a head it cannot write is not tried again by the next
/// resolve of the same log.
#[cfg(unix)]
#[test]
fn vault_head_is_written_from_the_full_read_without_reading_the_file_again() {
    let (log, macs, _, _) = inventory_log(4, 1024, 2);
    let identity = &macs[0].identity;
    let vault = Vault::of(&log);
    let store = vault.full_for(identity);
    let resolve = |store: &Store| {
        let _ = store
            .log()
            .unwrap()
            .open_audience_entries(identity)
            .unwrap();
    };
    std::fs::set_permissions(&vault.data, std::fs::Permissions::from_mode(0o000)).unwrap();
    let written = WRITES.get();
    resolve(&store);
    std::fs::set_permissions(&vault.data, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        WRITES.get(),
        written + 1,
        "written with the file unreadable"
    );
    assert!(vault.through_head(identity).is_some());

    // A folder the head cannot be written into: tried once.
    std::fs::remove_file(&vault.head).unwrap();
    let store = vault.full_for(identity);
    let folder = vault.data.parent().unwrap();
    std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o500)).unwrap();
    let tries = TRIES.get();
    resolve(&store);
    resolve(&store);
    std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(TRIES.get(), tries + 1, "one try for a log");
    assert!(!vault.head.exists());
}

/// A read that began within the file's own clock tick names no stamp in a head: a rewrite in
/// place at the same length in that tick (as a test's home snapshot writes one) keeps the stamp
/// and shows other bytes. Once the file has settled, the next full read writes it.
#[test]
fn vault_head_names_only_a_stamp_that_had_settled_before_the_read() {
    let (log, macs, _, _) = inventory_log(4, 1024, 2);
    let identity = &macs[0].identity;
    let vault = Vault::of(&log);
    let resolve = |store: &Store| {
        let _ = store
            .log()
            .unwrap()
            .open_audience_entries(identity)
            .unwrap();
    };
    let tries = TRIES.get();
    resolve(&vault.full_for_at(identity, std::time::SystemTime::now()));
    assert_eq!(TRIES.get(), tries, "a stamp within its tick: no head tried");
    assert!(!vault.head.exists());
    // NEGATIVE CONTROL: the same file, read once it settled.
    resolve(&vault.full_for(identity));
    assert!(vault.through_head(identity).is_some());
}

/// A head that names one entry by two numbers (no resolve writes one) is not trusted for it: the
/// entry's shelved ops are opened as a full resolve opens them. The guard's negative control is
/// the mutant without it, which reads two different values as one write.
#[test]
fn vault_head_entry_named_by_two_numbers_is_opened() {
    let (macs, entrants) = three_macs();
    let (mut log, epoch, key) = founded(&macs, &entrants);
    let put = |log: &mut Log, mac: &Mac, ms: u64, value: &[u8]| {
        let entry = Entry {
            name: "x".to_owned(),
            value: SecretBytes::from(value),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let draft = log.draft_put(&mac.member, ms, epoch, &key, &entry).unwrap();
        mac.sign(log, draft)
    };
    for ms in 2_000..2_004u64 {
        put(&mut log, &macs[0], ms, &ms.to_be_bytes());
    }
    let mut beside = log.clone();
    let one = put(&mut log, &macs[0], 2_010, b"one");
    let two = put(&mut beside, &macs[1], 2_010, b"two");
    log.insert(beside.get(&two).unwrap().clone()).unwrap();
    let identity = &macs[0].identity;
    let vault = Vault::of(&log);
    assert!(vault.written_by(identity));
    let full = vault.full();
    let mut read = vault.through_head(identity).unwrap();
    assert!(shelved(&read) > 0);
    let read_log = read.log.as_mut().unwrap();
    let ops = read_log.head.as_deref().unwrap();
    let mut forged = ops.ops.clone();
    let first = forged[&one];
    let fresh = forged
        .values()
        .map(|op| op.register)
        .filter(|&r| r != COMMONS)
        .max()
        .unwrap()
        + 1;
    forged.insert(
        two,
        Op {
            register: fresh,
            class: first.class,
        },
    );
    read_log.head = Some(Arc::new(Ops {
        recipient: ops.recipient,
        ops: forged,
    }));
    same_reading(
        full.log().unwrap(),
        read.log().unwrap(),
        identity,
        "two numbers",
    );
}

/// A Mac's home holding `log`'s records file and, where `identity` is one, its identity file.
fn home_of(
    log: &Log,
    identity: Option<&Identity>,
) -> (aterm_tempfile::TempDir, crate::client::Paths) {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = crate::client::Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    if let Some(identity) = identity {
        #[cfg(unix)]
        let file = {
            let mut file = identity.to_bech32().as_slice().to_vec();
            file.push(b'\n');
            file
        };
        // A Windows machine key is read only as this user's DPAPI sealed it.
        #[cfg(windows)]
        let file = crate::identity_file::stored_text(identity, None, 0)
            .unwrap()
            .as_slice()
            .to_vec();
        fsx::write_atomic(&paths.dir.join("identity"), &file, 0o600).unwrap();
    }
    fsx::write_atomic(&paths.records_file(), &encode_records(log.records()), 0o600).unwrap();
    (home, paths)
}

/// The records cache of this Mac's records file writes the head beside it at the resolve of a
/// full read under the identity in this Mac's identity file, and a later read of the file that
/// the cache does not hold reads through it; with no identity file it writes none.
#[test]
fn vault_head_is_written_and_read_through_this_macs_records_cache() {
    let (log, macs, _, _) = inventory_log(12, 16 * 1024, 2);
    let identity = &macs[0].identity;
    let (_home, paths) = home_of(&log, Some(identity));
    let bytes = std::fs::read(paths.records_file()).unwrap();
    let head = crate::records_storage::head_path(&paths.records_file()).unwrap();

    let load = |paths: &crate::client::Paths| {
        fsx::as_if_later(2 * fsx::SETTLED, || {
            crate::client::load_records(paths).unwrap()
        })
    };
    let store = load(&paths);
    assert_eq!(shelved(&store), 0, "a full read");
    assert!(!head.exists());
    let _ = store
        .log()
        .unwrap()
        .open_audience_entries(identity)
        .unwrap();
    assert!(head.exists(), "the resolve of the full read wrote the head");

    // The same bytes under a stamp this process's cache does not know.
    fsx::write_atomic(&paths.records_file(), &bytes, 0o600).unwrap();
    let read = load(&paths);
    assert!(shelved(&read) > 0, "read through the head");
    same_store(&store, &read, "the records cache");
    same_reading(
        store.log().unwrap(),
        read.log().unwrap(),
        identity,
        "the records cache",
    );

    // NEGATIVE CONTROL: a home with no identity file: the cache knows no identity to write a
    // head for, whoever resolves what it read.
    let (log, macs, _, _) = inventory_log(12, 16 * 1024, 2);
    let (_home, paths) = home_of(&log, None);
    let store = load(&paths);
    let _ = store
        .log()
        .unwrap()
        .open_audience_entries(&macs[0].identity)
        .unwrap();
    let head = crate::records_storage::head_path(&paths.records_file()).unwrap();
    assert!(!head.exists());
}

/// The open time with and without the head over a log shaped as the one measured on m263: one
/// entry of about 298 KB rewritten 165 times and 40 small ones, about 49 MB of which about 0.4 MB
/// is live. Prints the medians of five opens; asserts nothing of time.
#[test]
#[ignore = "a measurement: targo --unverified test -p aterm-secrets --lib -- vault_head_measure --ignored --nocapture"]
fn vault_head_measure_open_with_and_without_the_head() {
    let (log, macs, _, _) = inventory_log(165, 298_000, 40);
    let identity = &macs[0].identity;
    let vault = Vault::of(&log);
    let file_len = std::fs::metadata(&vault.data).unwrap().len();
    assert!(vault.written_by(identity));
    let median = |mut runs: Vec<std::time::Duration>| {
        runs.sort();
        runs[runs.len() / 2]
    };
    let time = |open: &dyn Fn() -> Store| {
        median(
            (0..5)
                .map(|_| {
                    let started = std::time::Instant::now();
                    let store = open();
                    let entries = store
                        .log()
                        .unwrap()
                        .open_entry_set(identity)
                        .unwrap()
                        .unwrap();
                    assert_eq!(entries.entries.len(), 41);
                    started.elapsed()
                })
                .collect(),
        )
    };
    let full = time(&|| {
        let bytes = std::fs::read(&vault.data).unwrap();
        let _ = digest(&bytes);
        Vault::empty().recovering(&bytes)
    });
    // A full open whose resolve writes the head: the first open after any append.
    let writing = time(&|| vault.full_for(identity));
    let headed = time(&|| vault.through_head(identity).unwrap());
    let read = vault.through_head(identity).unwrap();
    eprintln!(
        "vault_head: {} records, {file_len} bytes on file; full open and resolve {full:?}, \
         with the head written {writing:?}, through the head {headed:?}; kept in memory {} \
         bytes full, {} through the head",
        log.held_count(),
        vault.full().footprint(),
        read.footprint(),
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::Witness;
use super::*;
use crate::table::{
    Session,
    signatures::test_hashed,
    store::{CONFLICT_SUBJECT, Receipt, SUBJECT, Store},
};
use astream_wire::sync::Digest;
use aterm_journal::Log;
use aterm_secrets::commons::roster::Role;
use aterm_uds::private_file;
use std::{
    os::unix::fs::{FileExt as _, PermissionsExt as _},
    sync::{
        Condvar,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, UNIX_EPOCH},
};

const ANCHOR: &[u8] = b"a table journal's anchor, as its enrollment claim pins it";
/// Each synthetic record's wire.
const WIRE: usize = 1000;
/// A law's hang detector for what must happen.
const HANG: Duration = Duration::from_secs(60);

fn limits() -> Limits {
    Limits {
        records: 256,
        bytes: 1 << 20,
        record_bytes: 4096,
    }
}

/// Room enough that every table a law asks is kept.
fn roomy() -> Journals {
    Journals::new(KEPT_TABLES, 1 << 30, 1 << 30)
}

/// A table journal of synthetic records in its own private folder: what a read identifies, which
/// a store would go on to authenticate.
struct Journal {
    _root: aterm_tempfile::TempDir,
    dir: PathBuf,
    records: std::cell::Cell<u64>,
}

impl Journal {
    fn new(records: u64) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = root.path().join("table");
        private_file::create_private_dir(&dir).unwrap();
        let mut log = Log::open(&dir, NAME, ANCHOR, false, limits()).unwrap();
        log.finish_open().unwrap();
        drop(log);
        let journal = Self {
            _root: root,
            dir,
            records: std::cell::Cell::new(0),
        };
        journal.append(records);
        journal
    }

    /// Append `records` operation records and conflict certificates in turn, each acknowledged.
    fn append(&self, records: u64) {
        for _ in 0..records {
            let at = self.records.get();
            let subject = if at % 2 == 0 {
                SUBJECT
            } else {
                CONFLICT_SUBJECT
            };
            self.append_as(subject);
        }
    }

    /// Append one record under `subject`, its body a receipt time and a wire.
    fn append_as(&self, subject: &str) {
        let at = self.records.get();
        let mut log = Log::open(&self.dir, NAME, ANCHOR, true, limits()).unwrap();
        log.finish_open().unwrap();
        let mut body = (1_000 + at).to_le_bytes().to_vec();
        body.extend(std::iter::repeat_n(u8::try_from(at % 251).unwrap(), WIRE));
        log.append(subject, body).unwrap();
        self.records.set(at + 1);
    }

    /// Flip one bit of the acknowledged byte at `at` in place: the same inode, length and
    /// checkpoint.
    fn flip(&self, at: u64) {
        flip(&self.dir, at);
    }
}

fn flip(dir: &Path, at: u64) {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("table.astream"))
        .unwrap();
    let mut byte = [0];
    file.read_exact_at(&mut byte, at).unwrap();
    file.write_all_at(&[byte[0] ^ 1], at).unwrap();
}

/// What a replay answered: each entry's receipt, wire bytes, identifier and conflict flag, and the
/// replay's witness; or the refusal's text.
type Answer = std::result::Result<(Vec<(Receipt, Vec<u8>, Digest, bool)>, [u8; 40]), String>;

fn answer(replay: Result<Replay>) -> Answer {
    replay
        .map(|replay| {
            let entries = replay
                .entries
                .iter()
                .map(|entry| {
                    (
                        entry.receipt,
                        entry.wire.wire().to_vec(),
                        entry.wire.id(),
                        entry.conflict,
                    )
                })
                .collect();
            (entries, replay.witness.bytes())
        })
        .map_err(|error| error.to_string())
}

/// What an observer open answered under `claim` before tables were kept: the whole prefix read
/// afresh, and witnessed.
fn fresh(dir: &Path, claim: Witness) -> Answer {
    answer(
        aterm_journal::read_committed(dir, NAME, ANCHOR, limits(), None)
            .map_err(Error::from)
            .and_then(|committed| match committed {
                Committed::Rebuilt { records, .. } => replay::read_observed(records, claim),
                _ => panic!("a read with no known prefix replays from zero"),
            }),
    )
}

/// What `journals` answer an ask at `now` under `claim`.
fn asked(journals: &Journals, dir: &Path, claim: Witness, now: SystemTime) -> Answer {
    answer(
        journals
            .read_at(dir, ANCHOR, limits(), now)
            .and_then(|read| read.witnessed(claim)),
    )
}

/// The claims a law asks: none, the chain through each record of the journal as it reads now,
/// each of those with a bit of its hash flipped, and one and two records past it.
fn claims(dir: &Path) -> Vec<Witness> {
    let records = match aterm_journal::read_committed(dir, NAME, ANCHOR, limits(), None) {
        Ok(Committed::Rebuilt { records, .. }) => records,
        _ => Vec::new(),
    };
    let mut links = Vec::new();
    let _ = replay::identify_onto(&mut links, records);
    let chain: Vec<Witness> = std::iter::once(Witness::default())
        .chain(links.iter().map(|link| link.through))
        .collect();
    let with = |witness: Witness, edit: &dyn Fn(&mut [u8; 40])| {
        let mut bytes = witness.bytes();
        edit(&mut bytes);
        Witness::decode(&bytes).unwrap()
    };
    let mut claims = chain.clone();
    claims.extend(
        chain
            .iter()
            .skip(1)
            .map(|witness| with(*witness, &|bytes: &mut [u8; 40]| bytes[8] ^= 1)),
    );
    let last = *chain.last().unwrap();
    for past in [1, 2] {
        let count = last.count + past;
        claims.push(with(last, &|bytes: &mut [u8; 40]| {
            bytes[..8].copy_from_slice(&count.to_le_bytes());
        }));
    }
    claims
}

/// The chain through every record of the journal as it reads now: the claim a writer publishes.
fn whole(dir: &Path) -> Witness {
    let Ok(Committed::Rebuilt { records, .. }) =
        aterm_journal::read_committed(dir, NAME, ANCHOR, limits(), None)
    else {
        panic!("the journal reads");
    };
    let mut links = Vec::new();
    replay::identify_onto(&mut links, records).unwrap();
    links
        .last()
        .map_or_else(Witness::default, |link| link.through)
}

/// An hour after anything a law did: its journals stood settled at the ask.
fn hour_on() -> SystemTime {
    SystemTime::now() + Duration::from_secs(3600)
}

/// The earliest and the latest change the journal in `dir` shows.
fn changed(dir: &Path) -> (SystemTime, SystemTime) {
    let (first, last) = JournalSeen::of(dir, NAME).unwrap().test_changed_ns();
    let at = |ns: i128| UNIX_EPOCH + Duration::from_nanos(u64::try_from(ns).unwrap());
    (at(first), at(last))
}

fn reads(journals: &Journals) -> usize {
    journals.reads.load(Ordering::Relaxed)
}

fn before_read(journals: &Journals, hook: impl Fn(&Path) + Send + Sync + 'static) {
    *journals.before_read.lock().unwrap() = Some(Arc::new(hook));
}

fn after_read(journals: &Journals, hook: impl Fn(&Path) + Send + Sync + 'static) {
    *journals.after_read.lock().unwrap() = Some(Arc::new(hook));
}

/// The listed tables' folders and charges, once the charges are checked: they sum to what is
/// charged, which is within the bound; each table charges what it keeps; no more tables are
/// listed than may be.
fn accounting(journals: &Journals) -> Vec<(PathBuf, usize)> {
    let (charged, slots) = {
        let list = journals.list.lock().unwrap();
        let slots: Vec<_> = list
            .slots
            .iter()
            .map(|slot| (Arc::clone(&slot.table), slot.charge))
            .collect();
        (list.charged, slots)
    };
    assert!(slots.len() <= journals.tables, "within the tables kept");
    assert_eq!(
        charged,
        slots.iter().map(|(_, charge)| charge).sum::<usize>(),
        "what is charged is what the tables charge"
    );
    assert!(charged <= journals.bytes, "within the bound");
    slots
        .into_iter()
        .map(|(table, charge)| {
            let keeps = table
                .kept
                .lock()
                .unwrap()
                .as_ref()
                .map_or(0, |kept| super::charge(&kept.links));
            assert_eq!(charge, keeps, "a table charges what it keeps");
            (table.dir.clone(), charge)
        })
        .collect()
}

/// The folders of the tables that keep records.
fn keeping(journals: &Journals) -> Vec<PathBuf> {
    accounting(journals)
        .into_iter()
        .filter(|(_, charge)| *charge > 0)
        .map(|(dir, _)| dir)
        .collect()
}

/// A kept journal answers each ask under each claim as a fresh read of the whole prefix does,
/// whether it is answered from what is kept, read with the kept prefix, or read again whole: the
/// journal unchanged; appended to, and appended to again past a claim; its astream file rewritten
/// in place as it was; a byte flipped in place inside the prefix, and restored; replaced by
/// byte-identical copies (other inodes); its anchor or folder opened to others, or its checkpoint
/// given a second link, each then undone; a foreign record appended; and truncated below its
/// checkpoint; each asked as it changed and an hour on, twice, under no claim, the chain through
/// each record, each with a hash bit flipped, and claims one and two records past it. After every
/// ask the table charges what it keeps: a refused read of a kept table lets its charge go.
/// NEGATIVE CONTROLS: a witness that leaves the folder out (its slot holding the astream file's
/// `lstat`) answers the folder opened to others where a fresh read refuses; an extension answered
/// as unchanged (the kept links alone) answers the append without it; links kept without their
/// last record (the sealed append a claim may not yet witness) refuse the claim through it "table
/// history lost acknowledged custody records"; what is kept taken for a read without its charge
/// let go leaves the flipped byte's refused table charging what it no longer keeps.
#[test]
fn a_kept_table_journal_answers_every_claim_as_a_fresh_read() {
    fn mode(path: &Path, bits: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
    }
    let journals = roomy();
    let journal = Journal::new(3);
    let dir = journal.dir.clone();
    let check = |what: &str, claims: &[Witness]| {
        for claim in claims {
            for (when, now) in [
                ("as it changed", SystemTime::now()),
                ("an hour on", hour_on()),
                ("an hour on, again", hour_on()),
            ] {
                assert_eq!(
                    asked(&journals, &dir, *claim, now),
                    fresh(&dir, *claim),
                    "{what}, asked {when}, claiming {} records",
                    claim.count
                );
                accounting(&journals);
            }
        }
    };
    check("unchanged", &claims(&dir));
    journal.append(1);
    check("an append", &claims(&dir));
    journal.append(1);
    check("a second append", &claims(&dir));
    let asking = claims(&dir);

    let astream = dir.join("table.astream");
    let bytes = std::fs::read(&astream).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&astream)
        .unwrap()
        .write_all_at(&bytes, 0)
        .unwrap();
    check("the astream file rewritten in place as it was", &asking);
    journal.flip(40);
    check("a byte flipped in place inside the prefix", &asking);
    journal.flip(40);
    check("the byte restored", &asking);
    for name in ["table.anchor", "table.checkpoint", "table.astream"] {
        let bytes = std::fs::read(dir.join(name)).unwrap();
        private_file::write_private(&dir, name, &bytes).unwrap();
    }
    check("replaced by byte-identical copies", &asking);

    type Change = (&'static str, fn(&Path), fn(&Path));
    let changes: [Change; 3] = [
        (
            "the anchor opened to others",
            |dir| mode(&dir.join("table.anchor"), 0o644),
            |dir| mode(&dir.join("table.anchor"), 0o600),
        ),
        (
            "the folder opened to others",
            |dir| mode(dir, 0o755),
            |dir| mode(dir, 0o700),
        ),
        (
            "a second link to the checkpoint",
            |dir| std::fs::hard_link(dir.join("table.checkpoint"), dir.join("linked")).unwrap(),
            |dir| std::fs::remove_file(dir.join("linked")).unwrap(),
        ),
    ];
    for (what, make, undo) in changes {
        make(&dir);
        assert!(fresh(&dir, Witness::default()).is_err(), "{what}: refused");
        check(what, &asking);
        undo(&dir);
        check(&format!("{what}, undone"), &asking);
    }

    journal.append_as("commons/other/1");
    assert!(fresh(&dir, Witness::default()).is_err(), "a foreign record");
    check("a foreign record appended", &asking);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&astream)
        .unwrap();
    file.set_len(file.metadata().unwrap().len() / 2).unwrap();
    check("truncated below its checkpoint", &asking);
}

/// What a table keeps answers only asks under the anchor and limits it was read under: an ask of
/// the same unchanged folder under another anchor, or under narrower limits, is another table,
/// read and answered as a fresh read under them is (each refused), and the kept table still
/// answers its own asks with no read.
/// NEGATIVE CONTROL: a table found by its folder alone (`Table::is`) answers the other anchor
/// from what is kept where a fresh read refuses "journal anchor changed".
#[test]
fn a_kept_table_journal_answers_only_its_own_anchor_and_limits() {
    let journals = roomy();
    let journal = Journal::new(3);
    let dir = journal.dir.clone();
    let claim = whole(&dir);
    asked(&journals, &dir, claim, hour_on()).unwrap();
    let start = reads(&journals);
    assert_eq!(asked(&journals, &dir, claim, hour_on()), fresh(&dir, claim));
    assert_eq!(reads(&journals), start, "kept");
    let narrow = Limits {
        records: 2,
        ..limits()
    };
    for (what, anchor, limits) in [
        (
            "another anchor",
            &b"another enrollment's anchor"[..],
            limits(),
        ),
        ("narrower limits", ANCHOR, narrow),
    ] {
        let fresh = answer(
            aterm_journal::read_committed(&dir, NAME, anchor, limits, None)
                .map_err(Error::from)
                .and_then(|committed| match committed {
                    Committed::Rebuilt { records, .. } => replay::read_observed(records, claim),
                    _ => panic!("a read with no known prefix replays from zero"),
                }),
        );
        assert!(fresh.is_err(), "{what}: a fresh read refuses it");
        let answered = answer(
            journals
                .read_at(&dir, anchor, limits, hour_on())
                .and_then(|read| read.witnessed(claim)),
        );
        assert_eq!(answered, fresh, "{what}: answered as a fresh read");
        accounting(&journals);
    }
    assert_eq!(reads(&journals), start + 2, "each read as another table");
    assert_eq!(asked(&journals, &dir, claim, hour_on()), fresh(&dir, claim));
    assert_eq!(reads(&journals), start + 2, "still kept under its own");
    assert_eq!(keeping(&journals), [dir]);
}

/// A settled journal is answered from what is kept with nothing opened, read or hashed; a read
/// asked within the settled window keeps no witness, so the next ask reads again, but with the
/// kept prefix, which identifies no record; an append is read with it, and identifies only the
/// new record.
/// NEGATIVE CONTROLS: with no witness kept, the settled ask reads; with no kept prefix handed to
/// the read, the read within the window identifies every record again.
#[test]
fn a_settled_table_journal_is_neither_read_nor_hashed_again() {
    const N: usize = 5;
    let journals = roomy();
    let journal = Journal::new(N as u64);
    let dir = journal.dir.clone();
    let claim = whole(&dir);
    let (_, last) = changed(&dir);
    let ask = |now: SystemTime| {
        let (read, hashed) = (reads(&journals), test_hashed());
        let asked = journals.read_at(&dir, ANCHOR, limits(), now).unwrap();
        (asked, reads(&journals) - read, test_hashed() - hashed)
    };
    let inside = last + Duration::from_secs(1);
    let later = last + Duration::from_secs(3600);
    let (_, read, hashed) = ask(inside);
    assert_eq!((read, hashed), (1, N), "a first ask reads and identifies");
    let (_, read, hashed) = ask(inside);
    assert_eq!(
        (read, hashed),
        (1, 0),
        "an ask after one within the window reads, with the kept prefix"
    );
    let (_, read, hashed) = ask(later);
    assert_eq!((read, hashed), (1, 0), "a settled ask reads once more");
    let (first, read, hashed) = ask(later);
    assert_eq!(
        (read, hashed),
        (0, 0),
        "a settled journal is not read again"
    );
    let (second, ..) = ask(later);
    assert!(Arc::ptr_eq(&first.links, &second.links), "the kept links");
    assert_eq!(answer(second.witnessed(claim)), fresh(&dir, claim));

    journal.append(1);
    let claim = whole(&dir);
    let later = changed(&dir).1 + Duration::from_secs(3600);
    let (appended, read, hashed) = ask(later);
    assert_eq!(
        (read, hashed),
        (1, 1),
        "an append is read, and its record identified"
    );
    assert_eq!(answer(appended.witnessed(claim)), fresh(&dir, claim));
    let (_, read, _) = ask(later);
    assert_eq!(read, 0);
}

/// A change made once the witness was taken, in place where no inode, length or checkpoint
/// moves, is seen at the next ask: the witness was taken before the read, so the change moved
/// what the next one shows, and that ask reads and refuses as a fresh read does.
/// NEGATIVE CONTROL: the witness taken after the read keeps it, and the next ask is answered.
#[test]
fn a_change_after_the_witness_is_seen_at_the_next_ask() {
    let journals = roomy();
    let journal = Journal::new(3);
    let dir = journal.dir.clone();
    let claim = whole(&dir);
    let flipped = AtomicBool::new(false);
    after_read(&journals, move |dir| {
        if !flipped.swap(true, Ordering::Relaxed) {
            flip(dir, 40);
        }
    });
    let later = hour_on();
    assert!(
        asked(&journals, &dir, claim, later).is_ok(),
        "the read before the change"
    );
    let start = reads(&journals);
    let next = asked(&journals, &dir, claim, later);
    assert_eq!(reads(&journals) - start, 1, "the next ask reads");
    assert!(next.is_err());
    assert_eq!(next, fresh(&dir, claim));
}

/// A read through a copy of the journal swapped in for it keeps no witness: its folder's parent
/// renamed aside and back moves none of the times the witness holds, so the read is matched to
/// the witness by the files it opened, and once the journal returns it is read again.
/// NEGATIVE CONTROL: the witness kept without `JournalSeen::stands_for` answers the copy's shorter
/// prefix after the journal returns.
#[test]
fn a_table_journal_read_through_a_copy_swapped_in_is_not_kept() {
    let base = aterm_tempfile::tempdir().unwrap();
    let commons = base.path().join("commons");
    let dir = commons.join("table");
    let fake = base.path().join("fake");
    for folder in [&commons, &dir, &fake, &fake.join("table")] {
        private_file::create_private_dir(folder).unwrap();
    }
    let mut log = Log::open(&dir, NAME, ANCHOR, false, limits()).unwrap();
    log.finish_open().unwrap();
    log.append(SUBJECT, vec![1; 20]).unwrap();
    drop(log);
    for name in ["table.anchor", "table.checkpoint", "table.astream"] {
        let bytes = std::fs::read(dir.join(name)).unwrap();
        private_file::write_private(&fake.join("table"), name, &bytes).unwrap();
    }
    let mut log = Log::open(&dir, NAME, ANCHOR, true, limits()).unwrap();
    log.finish_open().unwrap();
    log.append(SUBJECT, vec![2; 20]).unwrap();
    drop(log);
    let journals = roomy();
    let aside = base.path().join("aside");
    {
        let (commons, fake, aside) = (commons.clone(), fake.clone(), aside.clone());
        let swapped = AtomicBool::new(false);
        before_read(&journals, move |_| {
            if !swapped.swap(true, Ordering::Relaxed) {
                std::fs::rename(&commons, &aside).unwrap();
                std::fs::rename(&fake, &commons).unwrap();
            }
        });
    }
    let later = hour_on();
    let copy = journals.read_at(&dir, ANCHOR, limits(), later).unwrap();
    assert_eq!(copy.links.len(), 1, "the read found the copy");
    std::fs::rename(&commons, &fake).unwrap();
    std::fs::rename(&aside, &commons).unwrap();
    let start = reads(&journals);
    let returned = journals.read_at(&dir, ANCHOR, limits(), later).unwrap();
    assert_eq!(
        returned.links.len(),
        2,
        "the journal is read again once it returns"
    );
    assert_eq!(reads(&journals) - start, 1);
}

/// A read asked within the settled window of any of the journal's changes keeps no witness,
/// though its earlier changes settled: a later change could leave every stamp as it was. Asked an
/// hour on, the first read keeps one.
/// NEGATIVE CONTROL: every witness taken as settled (`JournalSeen::stands_for` on the inodes
/// alone) answers the second early ask without a read.
#[test]
fn a_table_journal_read_before_it_settled_is_read_again() {
    let journals = roomy();
    let journal = Journal::new(3);
    let dir = journal.dir.clone();
    let (first, last) = changed(&dir);
    assert!(
        first < last,
        "the journal's files changed at distinct times"
    );
    // The first change has settled at this ask, the last has not.
    let soon = first + Duration::from_secs(2);
    let later = last + Duration::from_secs(3600);
    let start = reads(&journals);
    for _ in 0..2 {
        journals.read_at(&dir, ANCHOR, limits(), soon).unwrap();
    }
    assert_eq!(
        reads(&journals) - start,
        2,
        "an unsettled journal is read at each ask"
    );
    for _ in 0..2 {
        journals.read_at(&dir, ANCHOR, limits(), later).unwrap();
    }
    assert_eq!(
        reads(&journals) - start,
        3,
        "the first settled read is kept"
    );
}

/// A read of one table's journal holds no lock another table's ask waits on: while one ask is
/// paused before its read, another table's ask is answered, and the paused one, released, answers
/// as a fresh read does.
/// NEGATIVE CONTROL: the read run under the tables' list lock, taken before the `before_read` hook
/// that begins it, leaves the other ask waiting, and the hang detector fails it.
#[test]
fn table_journal_reads_do_not_wait_for_unrelated_tables() {
    let journals = roomy();
    let (x, y) = (Journal::new(3), Journal::new(2));
    let (started, observed) = mpsc::channel();
    let (release, blocked) = mpsc::channel::<()>();
    let (started, blocked) = (Mutex::new(started), Mutex::new(blocked));
    let paused = x.dir.clone();
    before_read(&journals, move |dir| {
        if dir == paused {
            started.lock().unwrap().send(()).unwrap();
            // Until the law releases it, which it does once the other ask finished or its hang
            // detector fired.
            let _ = blocked.lock().unwrap().recv();
        }
    });
    std::thread::scope(|scope| {
        let journals = &journals;
        let (x_dir, y_dir) = (x.dir.as_path(), y.dir.as_path());
        let first = scope.spawn(move || asked(journals, x_dir, whole(x_dir), hour_on()));
        observed.recv_timeout(HANG).unwrap();
        let (completed, completion) = mpsc::channel();
        let second = scope.spawn(move || {
            completed
                .send(asked(journals, y_dir, whole(y_dir), hour_on()))
                .unwrap();
        });
        let result = completion.recv_timeout(HANG);
        // Release the paused read before asserting, the negative control's included.
        drop(release);
        assert_eq!(first.join().unwrap(), fresh(x_dir, whole(x_dir)));
        second.join().unwrap();
        assert_eq!(
            result.expect("another table's ask could not finish"),
            fresh(y_dir, whole(y_dir))
        );
    });
}

/// Two asks of one table at once each read it: neither holds the table while it reads, so both
/// reach their read before either finishes (the second finding nothing kept, the first having
/// taken it), each answers as a fresh read does, and the table is listed once, charging what it
/// keeps.
/// NEGATIVE CONTROL: the table's lock held from before the `before_read` hook that begins the read
/// through it keeps the second ask from its read until the first finishes, and the wait for both
/// fails at its hang detector.
#[test]
fn asks_of_one_table_at_once_each_read_it_and_neither_waits() {
    let journals = roomy();
    let journal = Journal::new(4);
    let dir = journal.dir.clone();
    let claim = whole(&dir);
    // Asked within the settled window: kept with no witness, so each ask below reads.
    let inside = changed(&dir).1 + Duration::from_secs(1);
    asked(&journals, &dir, claim, inside).unwrap();
    let arrived = Arc::new((Mutex::new(0), Condvar::new()));
    let missed = Arc::new(AtomicBool::new(false));
    {
        let (arrived, missed) = (Arc::clone(&arrived), Arc::clone(&missed));
        before_read(&journals, move |_| {
            let (count, both) = &*arrived;
            let mut count = count.lock().unwrap();
            *count += 1;
            both.notify_all();
            let (count, waited) = both
                .wait_timeout_while(count, HANG, |count| *count < 2)
                .unwrap();
            drop(count);
            if waited.timed_out() {
                missed.store(true, Ordering::Relaxed);
            }
        });
    }
    let answers = std::thread::scope(|scope| {
        let (journals, dir) = (&journals, dir.as_path());
        let asks: Vec<_> = (0..2)
            .map(|_| scope.spawn(move || asked(journals, dir, claim, inside)))
            .collect();
        asks.into_iter()
            .map(|ask| ask.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(
        !missed.load(Ordering::Relaxed),
        "both asks reached their read before either finished"
    );
    for answer in answers {
        assert_eq!(answer, fresh(&dir, claim));
    }
    let listed = accounting(&journals);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, dir);
}

/// Kept tables stay within their bound, and a table yields its room only to a table asked twice
/// since it was last asked. With room for two tables of three: a cycle of three keeps the first
/// two and reads the third at each ask; the third asked again displaces the oldest of those
/// asked before its last ask; a table asked once takes only free room; a table over the
/// per-journal bound is never kept; a fifth table lets the least recently asked go; a kept table
/// grown past the per-journal bound, or past the room left in a cycle with another, keeps
/// nothing more; and a table let go while it was read, asked again and kept under its key, keeps
/// its own charge when the first read finishes, or is refused. After every ask what is charged is
/// what the tables charge, each what it keeps, within the bound; every answer is a fresh read's.
/// A table asked several times in a row is asked once, so a cycle asked in runs keeps what a cycle
/// of single asks does.
/// NEGATIVE CONTROLS: room made by letting the least recently asked table go reads every ask of
/// the cycle; no bound on what all charge charges past it; a slot found by its key, not its
/// table, charges the table asked again for the first read's records; each ask counted on its
/// own, those of one table in a row included, lets the third table asked again in a row displace
/// the first.
#[test]
fn kept_journals_stay_within_budget_and_yield_only_to_a_table_asked_again() {
    const K: u64 = 8;
    let ask = |journals: &Journals, journal: &Journal| {
        let start = reads(journals);
        let claim = whole(&journal.dir);
        assert_eq!(
            asked(journals, &journal.dir, claim, hour_on()),
            fresh(&journal.dir, claim)
        );
        accounting(journals);
        reads(journals) - start
    };
    let charge_of = |journal: &Journal| {
        let scratch = roomy();
        ask(&scratch, journal);
        accounting(&scratch)[0].1
    };
    let [a, b, c, d, e, g, p, q] = [K, K, K, K, K, 3 * K, K, K].map(Journal::new);
    let s = charge_of(&a);
    assert_eq!(charge_of(&c), s, "the tables are alike");
    // Room for two of them, and one of them twice over.
    let journals = Journals::new(4, 2 * s + s / 2, 2 * s);
    assert!(charge_of(&g) > 2 * s, "over the per-journal bound");

    // A cycle of three: the first two are kept, the third read at each ask.
    for round in 0..3 {
        let read = [&a, &b, &c].map(|journal| ask(&journals, journal));
        assert_eq!(
            read,
            if round == 0 { [1, 1, 1] } else { [0, 0, 1] },
            "round {round}"
        );
    }
    assert_eq!(keeping(&journals), [a.dir.clone(), b.dir.clone()]);
    // Asked again in a row, the third has still not been asked twice since either was.
    assert_eq!(ask(&journals, &c), 1);
    assert_eq!(keeping(&journals), [a.dir.clone(), b.dir.clone()]);
    // Asked again after another table, it has been since the first was: the first yields.
    assert_eq!([ask(&journals, &b), ask(&journals, &c)], [0, 1]);
    assert_eq!(keeping(&journals), [b.dir.clone(), c.dir.clone()]);
    // Asked once, a table takes only free room.
    assert_eq!(ask(&journals, &d), 1);
    assert_eq!(keeping(&journals), [b.dir.clone(), c.dir.clone()]);
    // Over the per-journal bound, never kept, however often asked.
    assert_eq!([ask(&journals, &g), ask(&journals, &g)], [1, 1]);
    assert_eq!(keeping(&journals), [b.dir.clone(), c.dir.clone()]);
    // A fifth table: the least recently asked goes, and its room is free.
    assert_eq!(accounting(&journals).len(), 4);
    assert_eq!(ask(&journals, &e), 1);
    let listed: Vec<_> = accounting(&journals)
        .into_iter()
        .map(|(dir, _)| dir)
        .collect();
    assert!(!listed.contains(&b.dir), "the least recently asked went");
    assert_eq!(keeping(&journals), [c.dir.clone(), e.dir.clone()]);
    // A kept table grown past the per-journal bound keeps nothing more.
    c.append(2 * K);
    assert!(charge_of(&c) > 2 * s);
    assert_eq!([ask(&journals, &c), ask(&journals, &c)], [1, 1]);
    assert_eq!(keeping(&journals), std::slice::from_ref(&e.dir));

    // A cycle of three asked in runs, each table several times in a row as a repair round opens
    // it: the run is one ask, so the first two are kept and only the third is read.
    let journals = Journals::new(4, 2 * s + s / 2, 2 * s);
    let [u, v, w] = [K, K, K].map(Journal::new);
    for round in 0..3 {
        let read = [&u, &v, &w].map(|journal| [0; 3].map(|_| ask(&journals, journal)));
        assert_eq!(
            read,
            if round == 0 {
                [[1, 0, 0], [1, 0, 0], [1, 1, 1]]
            } else {
                [[0; 3], [0; 3], [1, 1, 1]]
            },
            "round {round} in runs"
        );
        assert_eq!(keeping(&journals), [u.dir.clone(), v.dir.clone()]);
    }

    // A kept table grown past the room left, in a cycle with the other kept table.
    let journals = Journals::new(4, 2 * s + s / 2, 2 * s);
    assert_eq!([ask(&journals, &p), ask(&journals, &q)], [1, 1]);
    p.append(K / 2 + 1);
    let grown = charge_of(&p);
    assert!(
        grown <= 2 * s && s + grown > 2 * s + s / 2,
        "within the per-journal bound, past the room"
    );
    for _ in 0..2 {
        assert_eq!([ask(&journals, &p), ask(&journals, &q)], [1, 0]);
        assert_eq!(keeping(&journals), std::slice::from_ref(&q.dir));
    }

    // A table let go while it is read, asked again meanwhile and kept under its key, then the
    // first read finishing with more records than the second kept, or refused.
    for refused in [false, true] {
        let journals = Arc::new(Journals::new(4, 1 << 30, 1 << 30));
        let table = Journal::new(K);
        let others: Vec<_> = (0..4).map(|_| Journal::new(1)).collect();
        let fired = AtomicBool::new(false);
        let (inner, dir) = (Arc::downgrade(&journals), table.dir.clone());
        let others_dirs: Vec<_> = others.iter().map(|journal| journal.dir.clone()).collect();
        before_read(&journals, move |at| {
            if at != dir || fired.swap(true, Ordering::Relaxed) {
                return;
            }
            let journals = inner.upgrade().unwrap();
            for other in &others_dirs {
                journals
                    .read_at(other, ANCHOR, limits(), hour_on())
                    .unwrap();
            }
            journals.read_at(&dir, ANCHOR, limits(), hour_on()).unwrap();
            let mut log = Log::open(&dir, NAME, ANCHOR, true, limits()).unwrap();
            log.finish_open().unwrap();
            let subject = if refused {
                "commons/other/1"
            } else {
                CONFLICT_SUBJECT
            };
            let mut body = 7_u64.to_le_bytes().to_vec();
            body.extend([9; WIRE]);
            log.append(subject, body).unwrap();
        });
        let first = journals
            .read_at(&table.dir, ANCHOR, limits(), hour_on())
            .unwrap();
        assert_eq!(
            first.links.len(),
            usize::try_from(K).unwrap() + usize::from(!refused)
        );
        assert_eq!(first.refused.is_some(), refused);
        let listed = accounting(&journals);
        let again = listed
            .iter()
            .find(|(dir, _)| *dir == table.dir)
            .expect("asked again, listed again");
        assert!(again.1 > 0, "the table asked again keeps its own read");
        drop(others);
    }
}

/// A table answered from what it keeps is read again at every [`REVALIDATE`]th ask, as an ask
/// after a change is, so a fault that moves no stamp is seen within that many asks: kept and
/// unchanged, a read every [`REVALIDATE`] asks, each answering as before; a byte flipped in place
/// and the witness made to show the journal as it stands now (a fault no stamp shows), the
/// answers from what is kept until the [`REVALIDATE`]th ask, which refuses as a fresh read does.
/// NEGATIVE CONTROL: with no revalidation the [`REVALIDATE`]th ask is answered from what is kept,
/// with no read.
#[test]
fn a_kept_table_journal_is_read_again_every_revalidate_asks() {
    let journals = roomy();
    let journal = Journal::new(3);
    let dir = journal.dir.clone();
    let claim = whole(&dir);
    let later = hour_on();
    let kept = asked(&journals, &dir, claim, later);
    assert!(kept.is_ok());
    let start = reads(&journals);
    for ask in 1..=2 * REVALIDATE {
        assert_eq!(asked(&journals, &dir, claim, later), kept);
        assert_eq!(
            reads(&journals) - start,
            usize::try_from(ask / REVALIDATE).unwrap(),
            "ask {ask}: read at each {REVALIDATE}th"
        );
    }
    journal.flip(40);
    let refused = fresh(&dir, claim);
    assert!(refused.is_err());
    {
        let table = journals
            .list
            .lock()
            .unwrap()
            .slots
            .iter()
            .find(|slot| slot.table.dir == dir)
            .map(|slot| Arc::clone(&slot.table))
            .unwrap();
        let mut slot = table.kept.lock().unwrap();
        let kept = slot.as_mut().unwrap();
        kept.witnessed = JournalSeen::of(&dir, NAME);
    }
    for ask in 1..REVALIDATE {
        assert_eq!(
            asked(&journals, &dir, claim, later),
            kept,
            "ask {ask}: answered from what is kept"
        );
    }
    assert_eq!(asked(&journals, &dir, claim, later), refused);
}

/// A replay shares its immutable wires whether or not the journal cache retains the read.
/// An explicit owned copy can be edited without changing a retained wire.
/// NEGATIVE CONTROL: `Read::witnessed` copying wires changes their allocation identity.
#[test]
fn a_replay_shares_immutable_wires_with_or_without_cache_retention() {
    let journal = Journal::new(3);
    let dir = journal.dir.clone();
    let claim = whole(&dir);
    let wires = |links: &[Linked]| {
        links
            .iter()
            .map(|link| link.wire.wire().as_ptr())
            .collect::<Vec<_>>()
    };
    let entries = |replay: &Replay| {
        replay
            .entries
            .iter()
            .map(|entry| entry.wire.wire().as_ptr())
            .collect::<Vec<_>>()
    };
    let none = Journals::new(KEPT_TABLES, 0, 0);
    let read = none.read_at(&dir, ANCHOR, limits(), hour_on()).unwrap();
    let held = wires(&read.links);
    let replay = read.witnessed(claim).unwrap();
    assert_eq!(entries(&replay), held, "an uncached read shares its wires");

    let journals = roomy();
    let read = journals.read_at(&dir, ANCHOR, limits(), hour_on()).unwrap();
    let held = wires(&read.links);
    let replay = read.witnessed(claim).unwrap();
    assert_eq!(
        entries(&replay),
        held,
        "a kept read shares its immutable wires"
    );
    let mut owned: Vec<_> = replay
        .entries
        .into_iter()
        .map(|entry| entry.wire.wire().to_vec())
        .collect();
    assert!(
        owned.iter().all(|wire| !held.contains(&wire.as_ptr())),
        "owned bytes are independent of retained wires"
    );
    let original = owned[0].clone();
    owned[0][0] ^= 1;
    let again = journals.read_at(&dir, ANCHOR, limits(), hour_on()).unwrap();
    assert_eq!(wires(&again.links), held, "and stays kept");
    assert_eq!(
        again.links[0].wire.wire(),
        original,
        "an owned edit leaves retained bytes intact"
    );
}

/// Compare the hint against the independently decoded, witnessed journal, including the
/// single unwitnessed append, an impossible claim and a changed custody commitment.
#[test]
fn repair_ids_keep_the_observers_exact_receipt_boundary() {
    let journal = Journal::new(5);
    let journals = roomy();
    let links = journals
        .read_at(&journal.dir, ANCHOR, limits(), hour_on())
        .unwrap()
        .links;
    let mut claims = vec![Witness::default(), whole(&journal.dir)];
    claims.extend(links.iter().map(|link| link.through));
    let mut changed = whole(&journal.dir).bytes();
    changed[8] ^= 1;
    claims.push(Witness::decode(&changed).unwrap());
    for claim in claims {
        let expected = fresh(&journal.dir, claim).map(|(entries, _)| {
            entries
                .into_iter()
                .filter(|(_, _, _, conflict)| !conflict)
                .map(|(_, _, id, _)| id)
                .collect::<std::collections::BTreeSet<_>>()
        });
        let actual = journals
            .read_at(&journal.dir, ANCHOR, limits(), hour_on())
            .unwrap()
            .repair_ids(claim)
            .map_err(|error| error.to_string());
        assert_eq!(actual, expected);
    }
}

/// A journal read cannot publish a hint under a claim that changed during that read.
/// Removing the final custody check admits the stale IDs in this negative control.
#[test]
fn repair_ids_recheck_custody_after_the_journal_read() {
    use crate::table::store::{Region, WITNESS_AT, claim_name};
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let dir = lab._home.path().join("repair-hint");
    let mut store = Store::create(&session, &dir).unwrap();
    let scope = store.scope();
    store.commit_catalog(&session, b"hint").unwrap();
    let expected = store.inventory(&session).unwrap().copied().collect();
    drop(store);
    drop(session);
    let session = Session::observe(&bootstrap).unwrap();
    let claims = Region::Primary.claims(&session).unwrap();
    let name = claim_name(scope);
    let original = private_file::read_private(&claims, &name, super::super::CLAIM_LEN).unwrap();
    let mut changed = original.clone();
    changed[WITNESS_AT + 8] ^= 1;
    let journals = Arc::new(roomy());
    let (fault_claims, fault_name) = (claims.clone(), name.clone());
    *journals.after_read.lock().unwrap() = Some(Arc::new(move |_| {
        private_file::write_private(&fault_claims, &fault_name, &changed).unwrap();
    }));
    let result = with_own(Arc::clone(&journals), || {
        Store::repair_ids_region(&session, &dir, scope, Region::Primary)
    });
    assert!(result.is_err(), "MUTANT: stale custody published a hint");
    private_file::write_private(&claims, &name, &original).unwrap();
    *journals.after_read.lock().unwrap() = None;
    let hint = with_own(journals, || {
        Store::repair_ids_region(&session, &dir, scope, Region::Primary)
    })
    .unwrap();
    assert_eq!(
        hint,
        Some(expected),
        "CONTROL: restored custody is readable"
    );
}

/// A kept table holds ciphertext alone: a row's random marker is in no kept wire, the kept
/// prefix holds no bytes, and every other field kept is named here (a field added fails to
/// compile this law). CONTROL: the store's decrypted row holds the marker.
/// NEGATIVE CONTROLS: the kept prefix keeping its bytes (`Retain::Bytes`) fails it; a field added
/// to what a table keeps, or to a link, fails to compile it.
#[test]
fn a_kept_table_read_holds_no_plaintext() {
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let dir = lab._home.path().join("plain");
    let mut store = Store::create(&session, &dir).unwrap();
    let scope = store.scope();
    let marker = crate::crypto::random::<32>().unwrap();
    store.commit_catalog(&session, &marker).unwrap();
    drop(store);
    let journals = Arc::new(roomy());
    let observed = with_own(Arc::clone(&journals), || {
        Store::observe(&session, &dir, scope)
    })
    .unwrap();
    let holds = |bytes: &[u8]| bytes.windows(marker.len()).any(|window| window == marker);
    assert!(
        observed
            .catalog(&session)
            .unwrap()
            .iter()
            .any(|row| holds(row.payload())),
        "CONTROL: the decrypted row holds the marker"
    );
    let table = journals
        .list
        .lock()
        .unwrap()
        .slots
        .iter()
        .find(|slot| slot.table.dir == dir)
        .map(|slot| Arc::clone(&slot.table))
        .expect("the observed table is kept");
    let kept = table.kept.lock().unwrap();
    let Some(Kept {
        prefix,
        links,
        witnessed,
        hits,
    }) = &*kept
    else {
        panic!("the observed table keeps its read");
    };
    assert!(!prefix.retains_bytes(), "the kept prefix holds no bytes");
    // An `lstat` of each path and a count.
    let _: (&Option<JournalSeen>, &u32) = (witnessed, hits);
    assert!(!links.is_empty());
    for link in links.iter() {
        let Linked {
            wire,
            received_ms,
            conflict,
            through,
        } = link;
        assert!(!holds(wire.wire()), "a kept wire is ciphertext");
        // A time, a flag and a count with a hash.
        let _: (&u64, &bool, &Witness) = (received_ms, conflict, through);
    }
}

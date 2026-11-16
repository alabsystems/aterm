// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The byte bound as a budget by who authors (`budgets.rs`): where each class stops, what
//! classes a batch, an automatic writer's size limit and day budget, that all of it binds only
//! where this Mac authors (a [`Log`] and a [`Store`] take a peer's record at any fill, as
//! before), and that a [`Fill`] reads what a header-only census of the same records counts.

use super::*;
use crate::SecretBytes;
use crate::membership_log::budgets::{
    AUTOMATIC_PUT_BUDGET, AUTOMATIC_PUT_LIMIT, AUTOMATIC_STOP, Author, Authoring, Class, Fill,
    PERSON_STOP, Projected, SeatBytes, Signed, author_stop, size_words,
};
use crate::text::{Audience, Entry};
use zeroize::Zeroizing;

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;
const HOUR: u64 = 3_600_000;
/// The clock the tests read: a day and more past every record they sign as old.
const NOW: u64 = 1_800_000_000_000;

/// A fresh `[everyone]` epoch by `by` at `at`, held: its id and key.
fn mint(log: &mut Log, by: Mac, at: u64) -> (RecordId, Zeroizing<[u8; 32]>) {
    let (draft, key) = log
        .draft_epoch(&by.id(), at, EpochAudience::Everyone, 1)
        .unwrap();
    let epoch = log.keep(draft.sign(&by.seed(), by.incarnation()).unwrap());
    (*epoch.id(), key)
}

/// A `Put` by `by` at `at` of `name`, a value of `len` bytes, not inserted.
fn put(
    log: &Log,
    by: Mac,
    at: u64,
    epoch: &(RecordId, Zeroizing<[u8; 32]>),
    name: &str,
    len: usize,
) -> Record {
    let entry = Entry {
        name: name.to_owned(),
        value: SecretBytes::from(vec![b'v'; len].as_slice()),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    log.draft_put(&by.id(), at, epoch.0, &epoch.1, &entry)
        .unwrap()
        .sign(&by.seed(), by.incarnation())
        .unwrap()
}

const UPKEEP: Author<'static> = Author::Automatic("the upkeep");

/// `record`, a `Put` its author changed, as the batch `author` authors alone.
fn alone(record: &Record, author: Author<'_>) -> Authoring {
    Authoring::all(std::slice::from_ref(record), Class::of_author(author))
}

/// `record` as the batch an automatic writer authors alone.
fn automatic(record: &Record) -> Authoring {
    alone(record, UPKEEP)
}

/// The stops are shares of the byte bound: seven eighths for an automatic writer, fifteen
/// sixteenths for a person's write, and the bound itself for membership; 56 and 60 of the
/// production 64 MiB, and the same shares of a test's smaller bound.
#[test]
fn vault_fill_stops_are_seven_eighths_and_fifteen_sixteenths_of_the_bound() {
    assert_eq!(MAX_RECORDS_STREAM, 64 * MIB);
    assert_eq!((AUTOMATIC_STOP, PERSON_STOP), (56 * MIB, 60 * MIB));
    assert_eq!(
        Class::ALL,
        [Class::Automatic, Class::Person, Class::Membership],
        "in the order they stop"
    );
    assert_eq!(
        Class::ALL.map(author_stop),
        [56 * MIB, 60 * MIB, 64 * MIB],
        "the production stops"
    );
    RecordBudgets::within_bytes(MIB, || {
        assert_eq!(
            Class::ALL.map(author_stop),
            [896 * KIB, 960 * KIB, MIB],
            "the same shares of a test's bound"
        );
    });
    assert_eq!(
        author_stop(Class::Automatic),
        56 * MIB,
        "the seam is restored"
    );
}

/// What classes a write: who authors it. A `Put` is its author's write at any size and under
/// any name, so a credential is a person's write and never draws on the room past the person's
/// stop; the kinds that take a Mac out or let one in are membership, a `Name` is a person's, and
/// an `Epoch`, a `Wrap` and a `Put` carry what their batch is.
#[test]
fn vault_fill_classes_a_write_by_its_author() {
    for (kind, class) in [
        (Kind::Genesis, Some(Class::Membership)),
        (Kind::Admit, Some(Class::Membership)),
        (Kind::Remove, Some(Class::Membership)),
        (Kind::Delete, Some(Class::Membership)),
        (Kind::Name, Some(Class::Person)),
        // The key peers admit a Mac the log let in by: the last step of its admission.
        (Kind::Node, Some(Class::Membership)),
        (Kind::Epoch, None),
        (Kind::Wrap, None),
        (Kind::Put, None),
    ] {
        assert_eq!(Class::of_kind(kind), class, "{kind:?}");
    }
    assert_eq!(Class::of_author(Author::Person), Class::Person);
    assert_eq!(Class::of_author(UPKEEP), Class::Automatic);

    // A credential's Put, small, from a person: a person's write, refused past the person's
    // stop with room left below the bound.
    let mut log = found(&[(A, "m1"), (B, "m2")]);
    let epoch = mint(&mut log, A, NOW);
    let token = put(&log, A, NOW, &epoch, "gh_access_token", 400);
    let batch = alone(&token, Author::Person);
    assert_eq!(batch.class_from(Author::Person), Class::Person);
    assert_eq!(
        batch.refusal(PERSON_STOP - batch.bytes, 0, Author::Person),
        None,
        "CONTROL: up to the person's stop it is signed"
    );
    let refusal = batch
        .refusal(PERSON_STOP, 0, Author::Person)
        .expect("a credential past the person's stop");
    assert!(
        refusal.starts_with("this write was not signed: it is a person's write (")
            && refusal.contains("a person's write stops at 60 MiB of the vault log's 64 MiB")
            && refusal.ends_with(
                "a person's write has 0 bytes left, a membership write (a removal and the \
                 re-keys it owes, an admission and its node key, a delete) 4 MiB"
            ),
        "{refusal}"
    );
    // The same bytes as a removal's batch run to the bound.
    let removal = Authoring::all(std::slice::from_ref(&token), Class::Membership);
    assert_eq!(
        removal.refusal(MAX_RECORDS_STREAM - removal.bytes, 0, Author::Person),
        None
    );
    assert!(
        removal
            .refusal(MAX_RECORDS_STREAM, 0, Author::Person)
            .is_some()
    );
}

/// A person's batch is judged whole by the least critical record the person changed, and an
/// `Epoch`, a `Wrap` or a `Put` that only seals an entry again carries what the batch is: beside
/// a `Delete` it is membership, alone it is a person's write unless its author says its keys let
/// a Mac in. An automatic writer's batch is automatic whatever it holds.
#[test]
fn vault_fill_judges_a_batch_by_what_its_author_changed() {
    let mut log = found(&[(A, "m1"), (B, "m2")]);
    let before = log.clone();
    let epoch = mint(&mut log, A, NOW);
    let minted = log.get(&epoch.0).unwrap().clone();
    let small = put(&log, A, NOW, &epoch, "gh_access_token", 400);
    let large = put(&log, A, NOW, &epoch, "sheet.json", 100 * KIB);
    let delete = log
        .draft_delete(&A.id(), NOW, epoch.0, &epoch.1, "old_token")
        .unwrap()
        .sign(&A.seed(), A.incarnation())
        .unwrap();
    // What an edit's draft tells of each record: `changed` names the Puts the edit changed.
    let class_of = |changed: &'static [usize], author: Author<'static>| {
        let mut at = 0;
        move |record: &Record| {
            at += 1;
            match record.kind() {
                Kind::Put if !changed.contains(&(at - 1)) => None,
                Kind::Put => Some(Class::of_author(author)),
                kind => Class::of_kind(kind),
            }
        }
    };
    let written = Authoring::classed(
        &[minted.clone(), small.clone()],
        class_of(&[1], Author::Person),
    );
    assert_eq!(
        written.class(),
        Class::Person,
        "a credential the person changed is a person's write, its epoch with it"
    );
    assert_eq!(
        written.bytes,
        minted.bytes().len() + small.bytes().len(),
        "every record's bytes"
    );
    assert_eq!(written.put_bytes, small.bytes().len(), "the Put's alone");
    let resealed = Authoring::classed(
        &[minted.clone(), delete.clone(), large.clone()],
        class_of(&[], Author::Person),
    );
    assert_eq!(
        resealed.class(),
        Class::Membership,
        "a Delete with the reseal it owes is membership whole"
    );
    assert_eq!(
        (resealed.put_bytes, resealed.largest_put),
        (large.bytes().len(), 0),
        "the Put sealed again is counted, and is no Put its author changed"
    );
    assert_eq!(
        Authoring::classed(
            &[minted.clone(), delete, small.clone()],
            class_of(&[2], Author::Person)
        )
        .class(),
        Class::Person,
        "a Put the person changed beside the Delete makes it a person's write"
    );
    let keys = Authoring::classed(std::slice::from_ref(&minted), class_of(&[], Author::Person));
    assert_eq!(
        keys.class(),
        Class::Person,
        "an epoch that seals nothing is a person's write"
    );
    assert_eq!(
        keys.keys_as(Class::Membership).class(),
        Class::Membership,
        "unless its author says what the keys are for: an edit that lets a Mac in"
    );
    assert_eq!(
        written.keys_as(Class::Membership).class(),
        Class::Person,
        "which never raises a batch that holds a classed record"
    );
    // An automatic writer's batch is automatic whatever it holds, a batch of keys alone too.
    assert_eq!(keys.class_from(UPKEEP), Class::Automatic);
    assert_eq!(
        keys.keys_as(Class::Membership).class_from(UPKEEP),
        Class::Automatic
    );
    assert_eq!(resealed.class_from(UPKEEP), Class::Automatic);
    let mixed = Authoring::classed(&[minted, small, large.clone()], class_of(&[1], UPKEEP));
    assert_eq!(
        mixed.largest_put,
        mixed.put_bytes - large.bytes().len(),
        "the size limit judges the Put the writer changed, not the one it sealed again"
    );
    // A write that waits for a key is weighed by its size and class before any record of it is
    // signed, and its bytes are a Put's.
    let waiting = Authoring::default()
        .with_put(400, Class::Person)
        .with_put(20 * KIB, Class::Person);
    assert_eq!(waiting.class(), Class::Person);
    assert_eq!(
        (waiting.bytes, waiting.put_bytes, waiting.largest_put),
        (400 + 20 * KIB, 400 + 20 * KIB, 20 * KIB)
    );
    assert!(
        before
            .author_ask(&Authoring::default(), Author::Person, &A.id(), NOW)
            .is_ok(),
        "a batch of nothing asks nothing"
    );
}

/// An automatic writer signs no `Put` over 64 KiB, at any fill: a size limit, not a class. In
/// an empty log, far below every stop and with nothing signed that day, its 298 KiB `Put` is
/// refused in words that name the writer, the size and the limit; a `Put` of exactly the limit
/// is signed, and a person's own `Put` of any size is held to its class alone.
#[test]
fn vault_fill_refuses_an_automatic_put_over_64_kib_at_any_fill() {
    let mut log = found(&[(A, "m1"), (B, "m2")]);
    let epoch = mint(&mut log, A, NOW);
    let upkeep = Author::Automatic("the repository inventory upkeep");
    let large = put(&log, A, NOW, &epoch, "inventory.json", 298 * KIB);
    let error = log
        .author_ask(&alone(&large, upkeep), upkeep, &A.id(), NOW)
        .expect_err("an automatic Put over the limit");
    assert_eq!(error.kind(), crate::ErrorKind::MembershipCapacity);
    assert_eq!(
        error.to_string(),
        format!(
            "the repository inventory upkeep's write was not signed: one Put of it is {}, and an \
             automatic writer signs no Put over 64 KiB, at any fill of the vault log",
            size_words(large.bytes().len())
        )
    );
    assert!(
        log.held_bytes() < KIB && log.authored_bytes_since(&A.id(), 0) == 0,
        "CONTROL: the log is all but empty and the seat signed no Put: {}",
        log.held_bytes()
    );
    log.author_ask(&alone(&large, Author::Person), Author::Person, &A.id(), NOW)
        .expect("CONTROL: a person's own Put of that size is held to its class alone");

    // The limit is of the Put's wire bytes: one of exactly 64 KiB is signed, one byte more not.
    let overhead = put(&log, A, NOW, &epoch, "inventory.json", 0).bytes().len();
    let at = put(
        &log,
        A,
        NOW,
        &epoch,
        "inventory.json",
        AUTOMATIC_PUT_LIMIT - overhead,
    );
    assert_eq!(
        at.bytes().len(),
        AUTOMATIC_PUT_LIMIT,
        "CONTROL: at the limit"
    );
    log.author_ask(&alone(&at, upkeep), upkeep, &A.id(), NOW)
        .expect("a Put of the limit is signed");
    let over = put(
        &log,
        A,
        NOW,
        &epoch,
        "inventory.json",
        AUTOMATIC_PUT_LIMIT - overhead + 1,
    );
    assert!(
        log.author_ask(&alone(&over, upkeep), upkeep, &A.id(), NOW)
            .is_err(),
        "one byte over is not"
    );
    // A Put the writer only seals again (given no class) is not judged by the limit: its size
    // is no choice of the writer's. It is still counted in the batch and in the seat's day.
    let carried = Authoring::classed(std::slice::from_ref(&large), |_| None);
    assert_eq!(carried.put_bytes, large.bytes().len());
    log.author_ask(&carried, upkeep, &A.id(), NOW)
        .expect("a Put sealed again is not the writer's size");
}

/// The classes are asked where this Mac authors, never where a record comes in: a log and a
/// store past each stop take a peer's valid record of the class that stop refuses here, up to
/// the hard bound, which refuses as it always did. A store that refused one would hold other
/// records than the peer that sent it.
///
/// CONTROL, in the test: at each fill the same record, authored here, is refused by its class.
#[test]
fn vault_fill_never_refuses_a_peers_record_on_ingest() {
    RecordBudgets::within_bytes(MIB, || {
        let mut log = found(&[(A, "m1"), (B, "m2")]);
        let epoch = mint(&mut log, A, NOW);
        let mut peer = Vec::new();
        // B's records, as a peer's stream brings them: one that fills most of the room, then
        // an automatic writer's past the automatic stop and a person's past the person's stop.
        for (name, len, author, refused_here) in [
            ("sheet.csv", 850 * KIB, Author::Person, None),
            ("inventory.json", 60 * KIB, UPKEEP, Some(Class::Automatic)),
            ("sheet.csv", 60 * KIB, Author::Person, Some(Class::Person)),
        ] {
            let record = put(&log, B, NOW, &epoch, name, len);
            let asked = log.author_ask(&alone(&record, author), author, &B.id(), 0);
            match refused_here {
                None => asked.expect("CONTROL: below its stop an author signs it"),
                Some(class) => {
                    let error = asked.expect_err("CONTROL: an author here is refused");
                    assert_eq!(error.kind(), crate::ErrorKind::MembershipCapacity);
                    assert!(
                        error
                            .to_string()
                            .contains(&format!("it is {} (", class.write())),
                        "the refusal names the {class:?} class: {error}"
                    );
                }
            }
            assert_eq!(
                log.insert(record.clone()).expect("a peer's record"),
                Inserted::Held,
                "the log takes a peer's {len}-byte Put at {} of {MIB}",
                log.held_bytes()
            );
            peer.push(record);
        }
        assert!(
            log.held_bytes() > author_stop(Class::Person),
            "CONTROL: the log is past every stop but the bound: {}",
            log.held_bytes()
        );
        let past = put(&log, B, NOW, &epoch, "sheet.csv", 100 * KIB);
        let error = log.insert(past).expect_err("the hard bound, as before");
        assert_eq!(error.kind(), crate::ErrorKind::MembershipCapacity);
        assert!(error.to_string().contains("record bytes"), "{error}");

        // A store that held none of them ingests the whole stream.
        let mut store = Store::for_vault("team").expect("store");
        let stream: Vec<Record> = log.records().cloned().collect();
        store.ingest_records(stream).expect("a peer's stream");
        for record in &peer {
            assert!(store.holds(record.id()), "the store holds each of B's Puts");
        }
        assert_eq!(
            store.log().map(Log::held_bytes),
            Some(log.held_bytes()),
            "the store holds what the peer holds"
        );
    });
}

/// One record of a records stream, read from its frame header alone: the kind code, the signer,
/// its clock and the frame's length. Independent of [`Record`]'s readers.
fn census(wire: &[u8]) -> Vec<(u8, [u8; 48], u64, usize)> {
    let mut rows = Vec::new();
    let mut at = 0;
    while at < wire.len() {
        let frame = &wire[at..];
        assert_eq!(&frame[..4], b"ALM1");
        let kind = frame[37];
        let actor: [u8; 48] = frame[38..86].try_into().unwrap();
        let clock = 127 + 32 * usize::from(frame[126]);
        let authored = u64::from_be_bytes(frame[clock..clock + 8].try_into().unwrap());
        let body = u32::from_be_bytes(frame[clock + 8..clock + 12].try_into().unwrap());
        let len = clock + 12 + usize::try_from(body).unwrap() + 64;
        rows.push((kind, actor, authored, len));
        at += len;
    }
    assert_eq!(at, wire.len(), "the stream frames with no remainder");
    rows
}

/// A [`Fill`] is a header census: its bytes, its bytes by kind and what each seat signed in 24
/// hours equal what the frame headers of the same records count, and the status line says those
/// numbers. Three seats sign over two days; the two that signed most in the last day are named,
/// largest first, and what the third and the day before signed is in neither.
#[test]
fn vault_fill_status_line_matches_a_header_census() {
    let mut log = found(&[(A, "m1"), (B, "m2"), (C, "m3")]);
    let old = NOW - 30 * HOUR;
    let epoch = mint(&mut log, A, old);
    for (by, at, len) in [
        (A, old, 300 * KIB),
        (B, old + HOUR, 9 * KIB),
        (B, NOW - 23 * HOUR, 200 * KIB),
        (A, NOW - 2 * HOUR, 40 * KIB),
        (B, NOW - HOUR, 200 * KIB),
        (C, NOW - HOUR, 2 * KIB),
    ] {
        let record = put(&log, by, at, &epoch, "sheet.csv", len);
        log.keep(record);
    }
    log.keep(
        log.draft(
            &C.id(),
            NOW,
            Body::Name {
                name: "desk".to_owned(),
            },
        )
        .sign(&C.seed(), C.incarnation())
        .unwrap(),
    );
    let rows = census(&encode_records(log.records()));
    let fill = log.fill(Some(NOW));

    assert_eq!(fill.bytes, rows.iter().map(|row| row.3).sum::<usize>());
    assert_eq!(fill.bound, MAX_RECORDS_STREAM);
    for (kind, records, bytes) in fill.by_kind {
        let of_kind: Vec<usize> = rows
            .iter()
            .filter(|row| row.0 == kind.code())
            .map(|row| row.3)
            .collect();
        assert_eq!(
            (records, bytes),
            (of_kind.len(), of_kind.iter().sum()),
            "{kind:?}"
        );
    }
    let since = NOW - 24 * HOUR;
    let signed_by = |mac: Mac| -> usize {
        rows.iter()
            .filter(|row| row.0 != Kind::Genesis.code() && row.1 == mac.id() && row.2 >= since)
            .map(|row| row.3)
            .sum()
    };
    let (a, b, c) = (signed_by(A), signed_by(B), signed_by(C));
    assert!(
        b > a && a > c && c > 0,
        "CONTROL: B, then A, then C: {b} {a} {c}"
    );
    assert_eq!(
        fill.signed,
        Some(Signed {
            bytes: a + b + c,
            seats: vec![
                SeatBytes {
                    seat: B.id(),
                    bytes: b
                },
                SeatBytes {
                    seat: A.id(),
                    bytes: a
                },
                SeatBytes {
                    seat: C.id(),
                    bytes: c
                },
            ],
        })
    );
    let left = u128::try_from(MAX_RECORDS_STREAM - fill.bytes).unwrap();
    let ms = u64::try_from(left * 24 * u128::from(HOUR) / u128::try_from(a + b + c).unwrap());
    assert_eq!(fill.projected(), Projected::In(ms.unwrap()));
    let membership = log.membership();
    let label = |seat: &MemberId| membership.label(seat).unwrap_or("a Mac").to_owned();
    assert_eq!(
        fill.line(label),
        format!(
            "log: {} of 64 MiB · every class open · signed in 24 h: {} (m2 {}, m1 {}) · \
             full in: {} d",
            size_words(fill.bytes),
            size_words(a + b + c),
            size_words(b),
            size_words(a),
            ms.unwrap() / (24 * HOUR),
        )
    );
    // The budget an automatic writer is held to counts a seat's Puts alone, by the same headers.
    assert_eq!(
        log.authored_bytes_since(&C.id(), since),
        rows.iter()
            .filter(|row| row.0 == Kind::Put.code() && row.1 == C.id() && row.2 >= since)
            .map(|row| row.3)
            .sum::<usize>(),
    );
    assert!(
        log.authored_bytes_since(&C.id(), since) < c,
        "C's Name is no Put"
    );
    assert!(log.authored_bytes_since(&B.id(), since) < AUTOMATIC_PUT_BUDGET);

    // What is not measured says so: no clock, or a clock that read nothing.
    for unread in [log.fill(None), log.fill(Some(0))] {
        assert_eq!(unread.signed, None);
        assert_eq!(unread.projected(), Projected::Unread);
        assert!(
            unread
                .line(|_| String::new())
                .ends_with("signed in 24 h: unread · full in: unread"),
            "{}",
            unread.line(|_| String::new())
        );
    }
    // A day in which nothing was signed projects no fill.
    let quiet = log.fill(Some(NOW + 48 * HOUR));
    assert_eq!(quiet.projected(), Projected::Never);
    assert!(
        quiet
            .line(|_| String::new())
            .ends_with("signed in 24 h: 0 bytes · full in: not at this rate"),
        "{}",
        quiet.line(|_| String::new())
    );
}

/// A day's window ends at the reader's clock and ten minutes of skew: a record whose signer's
/// clock ran a year ahead is in no day's count, so one peer's wrong clock is not read as a rate
/// until a compaction takes its record away, and a `Put` this seat signed while its own clock
/// ran ahead is not held against an automatic writer for a year. A reader with no clock counts
/// every `Put` its seat ever signed: an automatic writer is then refused, never unbounded.
///
/// CONTROL, in the test: the same records stamped inside the window are counted.
#[test]
fn vault_fill_window_ends_at_the_readers_clock() {
    use crate::membership_log::budgets::WINDOW_SKEW_MS;
    let mut log = found(&[(A, "m1"), (B, "m2")]);
    let epoch = mint(&mut log, A, NOW - HOUR);
    let year = 365 * 24 * HOUR;
    let ahead = put(&log, B, NOW + year, &epoch, "sheet.csv", 700 * KIB);
    log.keep(ahead.clone());
    let skewed = put(&log, B, NOW + WINDOW_SKEW_MS, &epoch, "sheet.csv", 9 * KIB);
    log.keep(skewed.clone());
    let own = put(&log, A, NOW + year, &epoch, "sheet.csv", 1020 * KIB);
    log.keep(own.clone());
    let recent = put(&log, A, NOW - HOUR, &epoch, "sheet.csv", KIB);
    log.keep(recent.clone());

    let signed = log.fill(Some(NOW)).signed.expect("the clock read");
    let of = |mac: Mac| {
        signed
            .seats
            .iter()
            .find(|seat| seat.seat == mac.id())
            .map(|seat| seat.bytes)
    };
    assert_eq!(
        of(B),
        Some(skewed.bytes().len()),
        "B's record a year ahead is in no day; the one within the skew is"
    );
    assert_eq!(
        log.fill(Some(NOW + year)).signed.map(|signed| signed.bytes),
        Some(ahead.bytes().len() + own.bytes().len()),
        "CONTROL: a reader whose clock reached them counts both"
    );

    // The budget: the 1020 KiB this seat signed a year ahead and one 15 KiB Put pass 1 MiB.
    let small = put(&log, A, NOW, &epoch, "sheet.csv", 15 * KIB);
    let batch = automatic(&small);
    let upkeep = UPKEEP;
    assert!(
        own.bytes().len() + batch.put_bytes > AUTOMATIC_PUT_BUDGET
            && recent.bytes().len() + batch.put_bytes < AUTOMATIC_PUT_BUDGET,
        "CONTROL: only with the Put a year ahead is the seat past its budget"
    );
    log.author_ask(&batch, upkeep, &A.id(), NOW)
        .expect("the Put a year ahead is not in today's budget");
    let error = log
        .author_ask(&batch, upkeep, &A.id(), NOW + year)
        .expect_err("CONTROL: once the clock reaches it, it is");
    assert_eq!(error.kind(), crate::ErrorKind::MembershipCapacity);
    let error = log
        .author_ask(&batch, upkeep, &A.id(), 0)
        .expect_err("with no clock every Put the seat signed counts");
    assert!(
        error.to_string().contains("in the last 24 hours"),
        "{error}"
    );
    assert_eq!(
        log.authored_bytes_since(&A.id(), 0),
        own.bytes().len() + recent.bytes().len(),
        "what the seat ever signed, read with no upper edge"
    );
}

/// The line at the fill a header census of a full lab log found (53,071,025 bytes; 20,036,768
/// signed in a day, by two seats): every class open, 16 hours from the bound; then past the
/// automatic stop, past the person's stop, and at the bound.
#[test]
fn vault_fill_status_line_names_the_refused_classes_and_the_time_left() {
    let fill = |bytes: usize| Fill {
        bytes,
        bound: MAX_RECORDS_STREAM,
        by_kind: Kind::ALL.map(|kind| (kind, 0, 0)),
        stops: Class::ALL.map(author_stop),
        signed: Some(Signed {
            bytes: 20_036_768,
            seats: vec![
                SeatBytes {
                    seat: A.id(),
                    bytes: 13_756_246,
                },
                SeatBytes {
                    seat: B.id(),
                    bytes: 6_280_522,
                },
            ],
        }),
    };
    let label = |seat: &MemberId| if *seat == A.id() { "m22" } else { "m2" }.to_owned();
    let census = fill(53_071_025);
    assert_eq!(
        census.line(label),
        "log: 50.61 of 64 MiB · every class open · signed in 24 h: 19.1 MiB (m22 13.11 MiB, m2 \
         5.98 MiB) · full in: 16 h"
    );
    assert_eq!(census.room(Class::Automatic), 56 * MIB - 53_071_025);
    assert_eq!(census.room(Class::Person), 60 * MIB - 53_071_025);
    assert_eq!(census.room(Class::Membership), 64 * MIB - 53_071_025);
    let past = fill(57 * MIB);
    assert!(
        past.line(label).contains("· automatic refused ·"),
        "{}",
        past.line(label)
    );
    assert!(!past.open(Class::Automatic) && past.open(Class::Person));
    assert_eq!(past.room(Class::Automatic), 0);
    let reserve = fill(61 * MIB);
    assert!(
        reserve
            .line(label)
            .contains("· automatic and person refused ·"),
        "{}",
        reserve.line(label)
    );
    assert!(!reserve.open(Class::Person) && reserve.open(Class::Membership));
    assert_eq!(reserve.room(Class::Membership), 3 * MIB);
    let full = fill(64 * MIB);
    assert!(full.line(label).contains("· full: every class refused ·"));
    assert!(full.line(label).ends_with("full in: 0 min"));
    assert_eq!(
        fill(12 * KIB + 300).line(label).split(" · ").next(),
        Some("log: 12.2 KiB of 64 MiB"),
        "each size keeps its own unit where they differ"
    );
}

/// A refusal names the class, the fill and what still fits, and an automatic writer's budget
/// names what the seat signed; neither holds any entry's name or value.
#[test]
fn vault_fill_refusal_names_the_class_the_fill_and_what_still_fits() {
    let mut log = found(&[(A, "m1"), (B, "m2")]);
    let epoch = mint(&mut log, A, NOW);
    let record = put(&log, A, NOW, &epoch, "inventory.json", 15 * KIB);
    let batch = automatic(&record);
    let upkeep = Author::Automatic("the repository inventory upkeep");
    assert_eq!(batch.refusal(AUTOMATIC_STOP - batch.bytes, 0, upkeep), None);
    let refusal = batch
        .refusal(57 * MIB + MIB / 2, 0, upkeep)
        .expect("past the automatic stop");
    assert_eq!(
        refusal,
        format!(
            "the repository inventory upkeep's write was not signed: it is an automatic write \
             (one no person started: the repository inventory's upkeep, the account broker's \
             publish, a re-key nothing waits on) of {}, and an automatic write stops at 56 MiB \
             of the vault log's 64 MiB, which holds 57.5 MiB; an automatic write has 0 bytes \
             left, a person's write (a person's own Put, a credential's too, or a name) 2.5 MiB, \
             a membership write (a removal and the re-keys it owes, an admission and its node \
             key, a delete) 6.5 MiB",
            size_words(batch.bytes)
        )
    );
    // The same Put from a person is a person's write, and fits.
    let own = alone(&record, Author::Person);
    assert_eq!(
        own.refusal(57 * MIB + MIB / 2, usize::MAX, Author::Person),
        None
    );
    assert!(
        own.refusal(PERSON_STOP, 0, Author::Person)
            .is_some_and(|why| why.starts_with(
                "this write was not signed: it is a person's write (a person's own Put, a \
                 credential's too, or a name) of "
            ))
    );
    // The seat's budget: a day's Puts and this one's pass 1 MiB.
    assert_eq!(
        batch.refusal(0, AUTOMATIC_PUT_BUDGET - batch.put_bytes, upkeep),
        None
    );
    let refusal = batch
        .refusal(0, AUTOMATIC_PUT_BUDGET - 4 * KIB, upkeep)
        .expect("past the seat's budget");
    assert_eq!(
        refusal,
        format!(
            "the repository inventory upkeep's write was not signed: this Mac's seat signed \
             1020 KiB of Puts in the last 24 hours, and this one's {} would pass the 1 MiB an \
             automatic writer may sign in a day, which leaves it 4 KiB; a person's own write is \
             not held to it",
            size_words(batch.put_bytes)
        )
    );
    assert_eq!(
        batch.refusal(0, usize::MAX, Author::Person),
        None,
        "a person's write is never held to the budget"
    );
}

/// A draft's clone that holds its batch already is asked as the log was before it
/// ([`Log::authored_ask`]): the batch is weighed once, against the fill and, from an automatic
/// writer, against its seat's day, never twice. A batch that takes the log to its stop is signed,
/// and so is one that takes the seat's day to its budget.
///
/// CONTROL, in the test: the same batch asked as one more beside the log that holds it
/// ([`Log::author_ask`]) is refused at both edges.
#[test]
fn vault_fill_weighs_a_drafted_batch_once() {
    let mut log = found(&[(A, "m1"), (B, "m2")]);
    let epoch = mint(&mut log, A, NOW);
    // The seat's day, all but what the batch takes: one Put a little short of the budget.
    let earlier = put(&log, A, NOW - HOUR, &epoch, "sheet.csv", MIB - 15 * KIB);
    log.keep(earlier.clone());
    let record = put(&log, A, NOW, &epoch, "sheet.csv", 10 * KIB);
    log.keep(record.clone());
    let held = log.held_bytes();
    let bytes = record.bytes().len();

    // The person's stop at most 15 bytes past what the log holds with the batch in it.
    RecordBudgets::within_bytes((held / 15 + 1) * 16, || {
        let stop = author_stop(Class::Person);
        assert!(
            held <= stop && held + bytes > stop,
            "CONTROL: the batch takes the log to its stop: {held} of {stop}"
        );
        let batch = alone(&record, Author::Person);
        log.authored_ask(&batch, Author::Person, &A.id(), NOW)
            .expect("the drafted batch, weighed once");
        let error = log
            .author_ask(&batch, Author::Person, &A.id(), NOW)
            .expect_err("CONTROL: weighed twice");
        assert_eq!(error.kind(), crate::ErrorKind::MembershipCapacity);
    });

    let signed = earlier.bytes().len() + bytes;
    assert!(
        signed <= AUTOMATIC_PUT_BUDGET && signed + bytes > AUTOMATIC_PUT_BUDGET,
        "CONTROL: the batch takes the seat's day to its budget: {signed}"
    );
    let batch = automatic(&record);
    log.authored_ask(&batch, UPKEEP, &A.id(), NOW)
        .expect("the drafted batch, counted once in the seat's day");
    let error = log
        .author_ask(&batch, UPKEEP, &A.id(), NOW)
        .expect_err("CONTROL: counted twice");
    assert!(
        error.to_string().contains("in the last 24 hours"),
        "{error}"
    );
}

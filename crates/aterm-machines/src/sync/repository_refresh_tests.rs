// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn scope(byte: u8) -> RefreshScope {
    RefreshScope::new([byte; 32])
}

fn pass(memory: &Arc<Memory>, now: u64) -> (RefreshObservation, Arc<RepositoryPass>) {
    let access = RepositoryPass::memory(memory, now);
    (access.observed, access.pass.expect("the lease is free"))
}

#[cfg(unix)]
#[test]
fn repeated_shared_wait_keeps_the_receipt_until_pending_evidence_changes() {
    use std::os::unix::fs::MetadataExt as _;

    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = record_path(scratch.path());
    let access = RepositoryPass::open(scratch.path(), 1_000).unwrap();
    let pass = access.pass.unwrap();
    let source = scope(1);
    let request = RefreshRequest::ensure(Some(source), access.observed.version);
    let Admission::Refresh(ticket) = pass.admit(request, source, 1_000).unwrap() else {
        panic!("a new source owes a refresh");
    };
    pass.finish(ticket, false, 2_000).unwrap();

    // Hold the old inode open so an atomic replacement cannot reuse it.
    let before = std::fs::File::open(&path).unwrap();
    let inode = before.metadata().unwrap().ino();
    let observation = observe(scratch.path(), 2_001);
    assert!(matches!(
        pass.admit(request, source, 2_001).unwrap(),
        Admission::Waiting
    ));
    assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(observe(scratch.path(), 2_001), observation);

    let updated = RefreshRequest::ensure(Some(source), observation.version);
    assert_ne!(updated, request);
    assert!(matches!(
        pass.admit(updated, source, 2_001).unwrap(),
        Admission::Waiting
    ));
    assert_ne!(std::fs::metadata(&path).unwrap().ino(), inode);
    let after = observe(scratch.path(), 2_001);
    assert_eq!(after.pending, Some(updated));
    assert_eq!(after.retry_ms, observation.retry_ms);
    assert_eq!(after.version, observation.version);
    drop(before);
}

/// A request that names inputs which moved before its runner admitted it is admitted for the
/// inputs the runner validated: it owns a wave of that scope under a later input, and once
/// that wave is complete the same stale request is fulfilled by it rather than run again.
/// NEGATIVE CONTROL: the refusal of a request whose scope differs from the runner's (0.200.0)
/// makes the first admission an error; a wave that kept the request's scope fails the scope
/// assertion.
#[test]
fn a_request_made_before_its_inputs_moved_is_admitted_for_the_inputs_there_are() {
    let memory = Arc::new(Memory::default());
    let (old, new) = (scope(1), scope(2));
    let (observed, first) = pass(&memory, 1_000);
    let Admission::Refresh(ticket) = first
        .admit(
            RefreshRequest::ensure(Some(old), observed.version),
            old,
            1_000,
        )
        .expect("the first wave")
    else {
        panic!("nothing was fulfilled yet");
    };
    first
        .finish(ticket, true, 2_000)
        .expect("the first wave ends");
    let settled = first.record().repositories.expect("recorded").version;

    let stale = RefreshRequest::inputs(old, observed.version);
    let Admission::Refresh(ticket) = first
        .admit(stale, new, 3_000)
        .expect("admitted, not refused")
    else {
        panic!("the moved inputs owe a wave");
    };
    assert!(ticket.scope() == new, "the wave is the runner's own scope");
    let admitted = first.record().repositories.expect("recorded").version;
    assert!(admitted.input > settled.input, "the move is a new input");
    first
        .finish(ticket, true, 4_000)
        .expect("the second wave ends");

    assert!(
        matches!(
            first.admit(stale, new, 5_000).expect("admitted"),
            Admission::Fulfilled
        ),
        "the same stale request is answered by the wave that ran for it"
    );
}

/// Every string a value holds, in order.
fn strings(value: &aterm_json::Value, into: &mut Vec<String>) {
    match value {
        aterm_json::Value::String(text) => into.push(text.clone()),
        aterm_json::Value::Array(items) => {
            for item in items {
                strings(item, into);
            }
        }
        aterm_json::Value::Object(fields) => {
            for field in fields.values() {
                strings(field, into);
            }
        }
        _ => {}
    }
}

/// The record a pass keeps of its own checkout writes (`own`) holds digests alone: the folder a
/// write names its checkout by and the checkout as the observer read it are 64 lowercase
/// hexadecimal digits each, no path the pass wrote under appears anywhere in the record, and
/// the record reads back as written. A run of finished writes takes a checkout from where the
/// observer last read it to where it reads it now ([`CheckoutWrites::account`]), an open one
/// only while its pass's process lives, and a write that begins where the last one did not end
/// lets the earlier ones go.
/// NEGATIVE CONTROL: a `CheckoutDigest` serialized as the folder's path puts the path in the
/// record and fails the first assertion on the text.
#[test]
fn the_receipt_of_a_passs_own_checkout_writes_holds_digests_only() {
    let memory = Arc::new(Memory::default());
    let (_, lease) = pass(&memory, 1_000);
    let folder = Path::new("/receipt-home/private-repository");
    let checkout = CheckoutDigest::folder(folder);
    let read = |byte: u8| CheckoutDigest::new([byte; 32]);
    let holder = Holder {
        pid: 4_242,
        started_s: 1_700,
    };

    let open = lease
        .begin_write(Some(holder), checkout, read(1))
        .expect("begun");
    let begun = lease.record().repositories.expect("recorded").own;
    assert_eq!(
        begun.account(checkout, read(1), read(2), true),
        Accounted::Running,
        "an effect under way while its pass runs"
    );
    assert_eq!(
        begun.account(checkout, read(1), read(2), false),
        Accounted::Not,
        "an effect left open by a pass that no longer runs accounts for nothing"
    );
    lease
        .end_write(open, Some((read(2), false)))
        .expect("ended");
    let open = lease
        .begin_write(Some(holder), checkout, read(2))
        .expect("begun");
    lease.end_write(open, Some((read(3), true))).expect("ended");
    let open = lease
        .begin_write(Some(holder), checkout, read(3))
        .expect("begun");
    lease.end_write(open, None).expect("ended");

    let record = lease.record();
    let own = record.repositories.clone().expect("recorded").own;
    assert_eq!(
        own.account(checkout, read(1), read(3), false),
        Accounted::Own { made: true }
    );
    assert_eq!(
        own.account(checkout, read(1), read(2), false),
        Accounted::Own { made: false }
    );
    assert_eq!(
        own.account(checkout, read(2), read(3), false),
        Accounted::Own { made: true }
    );
    assert_eq!(
        own.account(checkout, read(3), read(4), false),
        Accounted::Not,
        "a move after the writes is no write of the pass's"
    );
    assert_eq!(
        own.account(
            CheckoutDigest::folder(Path::new("/receipt-home/another-repository")),
            read(1),
            read(2),
            false
        ),
        Accounted::Not,
        "another checkout's move is not this one's write"
    );

    let text = aterm_json::to_string(&record).expect("serialized");
    assert!(
        !text.contains("receipt-home") && !text.contains("private-repository"),
        "{text}"
    );
    let mut digests = Vec::new();
    strings(
        &aterm_json::to_value(&own).expect("serialized"),
        &mut digests,
    );
    assert_eq!(
        digests.len(),
        2 * 3,
        "two kept writes, each a checkout, a before and an after; the one whose writes were \
         not the pass's own is not kept: {digests:?}"
    );
    assert!(
        digests.iter().all(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }),
        "{digests:?}"
    );
    let back: Record = aterm_json::from_slice(text.as_bytes()).expect("read back");
    assert_eq!(back.repositories.map(|state| state.own), Some(own));

    let open = lease
        .begin_write(Some(holder), checkout, read(7))
        .expect("begun");
    lease
        .end_write(open, Some((read(8), false)))
        .expect("ended");
    let own = lease.record().repositories.expect("recorded").own;
    assert_eq!(
        own.account(checkout, read(1), read(3), false),
        Accounted::Not,
        "a write that begins elsewhere lets the earlier ones go"
    );
    assert_eq!(
        own.account(checkout, read(7), read(8), false),
        Accounted::Own { made: false }
    );
}

/// Optional cached observations share the actual receipt's byte budget. A large cohort is
/// reduced without losing a fulfilled refresh, and later generic rows can reduce it further.
/// An oversized mandatory record preserves the last readable on-disk receipt.
#[test]
fn empty_observations_fit_the_record_reader_without_losing_scheduling_progress() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let access = RepositoryPass::open(scratch.path(), 1_000).unwrap();
    let owner = access.pass.unwrap();
    let scope = scope(3);
    let Admission::Refresh(ticket) = owner
        .admit(
            RefreshRequest::ensure(Some(scope), access.observed.version),
            scope,
            1_000,
        )
        .unwrap()
    else {
        panic!("the initial refresh is owed");
    };
    owner.finish(ticket, true, 2_000).unwrap();
    let observed = observe(scratch.path(), 2_000);
    let subjects: Vec<_> = (0_u64..10_000)
        .map(|number| {
            let mut hash = aterm_digest::Sha256::new();
            hash.update(number.to_le_bytes());
            let key = CheckoutDigest::new(hash.finalize());
            (key, key)
        })
        .collect();
    owner.record_empty(scope, subjects.clone()).unwrap();
    let held = owner.record();
    let retained = held.repositories.as_ref().unwrap().empty.subjects.len();
    assert!(
        retained > 0 && retained < subjects.len(),
        "only evidence that fits is retained"
    );
    assert_eq!(observe(scratch.path(), 2_000), observed);
    assert!(
        std::fs::metadata(record_path(scratch.path()))
            .unwrap()
            .len()
            <= super::super::RECORD_BYTES as u64
    );
    let mut newer = held;
    newer.build = "x".repeat(super::super::RECORD_BYTES / 2);
    owner.record_pass(newer).unwrap();
    let next = owner.record();
    assert!(next.repositories.as_ref().unwrap().empty.subjects.len() < retained);
    assert_eq!(observe(scratch.path(), 2_000), observed);
    assert!(super::super::read_record(scratch.path()).is_some());
    let before = std::fs::read(record_path(scratch.path())).unwrap();
    let mut oversized = next;
    oversized.build = "x".repeat(super::super::RECORD_BYTES);
    assert!(owner.record_pass(oversized).is_err());
    assert_eq!(std::fs::read(record_path(scratch.path())).unwrap(), before);
    assert_eq!(observe(scratch.path(), 2_000), observed);
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Large credential queues retain causal merges without inventing protocol actions.

use super::*;
use crate::table::tests::frontier::{first_records, receive, wide_lab, wires};
use astream_wire::sync::MAX_PARENTS;

/// With 65 writers' requests in the queue no record goes inert, the next pass adds a bounded merge
/// and the new request, a retry posts, answers, offers and sweeps nothing and appends nothing, and
/// a replica converges on the same requests. Macs 0, 1 and the last, past `LEGACY_MAX_MEMBERS`,
/// raise and post through the product; the others seal the record a first pass posts, so a defect
/// in one far member's own pass shows only there. NEGATIVE CONTROL: with `commit` calling
/// `commit_catalog`, Mac 0's later pass fails `table frontier requires a bounded merge`.
#[test]
fn a_wide_credential_queue_keeps_requests_and_retries_without_phantom_actions() {
    let writers = MAX_PARENTS + 1;
    let lab = wide_lab(writers);
    let now = now_ms();
    let last = writers - 1;
    for writer in [0, 1, last] {
        let paths = &lab.paths[writer];
        need::test_alab_policy(paths);
        need::raise(
            paths,
            Want::Mailbox(format!("frontier-{writer}")),
            Purpose::ByHand,
        )
        .unwrap();
        assert_eq!(pass_at(paths, now).unwrap().posted, 1);
    }
    let scope = descriptor(&lab.bootstrap(0)).unwrap().scope;
    let mut incoming = wires(&lab, 1, scope);
    incoming.extend(wires(&lab, last, scope));
    incoming.extend(first_records(&lab, 2..last, scope, |writer, session| {
        let nonce = [u8::try_from(writer).unwrap(); 16];
        let mut body = Map::new();
        body.insert("nonce".into(), Value::from(hex::encode_lower(&nonce)));
        body.insert(
            "want".into(),
            Value::from(Want::Mailbox(format!("frontier-{writer}")).key()),
        );
        body.insert("purpose".into(), Value::from(Purpose::ByHand.id()));
        encode(
            &Record::new("request", &request_key(&session.actor(), &nonce), body)
                .command()
                .unwrap(),
        )
        .unwrap()
    }));
    receive(&lab, 0, scope, &incoming);
    let before = observe(&lab.paths[0]).unwrap();
    assert_eq!(before.book.threads.len(), writers);
    assert!(before.book.inert.is_empty());
    assert_eq!(before.retained_operations, writers);
    let prior = before.book.threads.keys().copied().collect::<BTreeSet<_>>();

    let next = need::raise(
        &lab.paths[0],
        Want::Mailbox("after-frontier".into()),
        Purpose::ByHand,
    )
    .unwrap();
    assert_eq!(pass_at(&lab.paths[0], now).unwrap().posted, 1);
    let after = observe(&lab.paths[0]).unwrap();
    assert_eq!(after.book.threads.len(), writers + 1);
    assert!(after.book.inert.is_empty());
    assert!(prior.is_subset(&after.book.threads.keys().copied().collect()));
    assert!(
        after
            .book
            .threads
            .contains_key(&request_key(&after.observer, &next.id))
    );
    assert_eq!(after.retained_operations, after.interpreted_operations);
    let merges = crate::documents::application::observe_store(
        &lab.paths[0],
        descriptor,
        |session, store, _| {
            Ok(store
                .catalog(session)?
                .iter()
                .filter(|record| {
                    matches!(Command::decode(record.payload()), Ok(Command::Merge { .. }))
                })
                .count())
        },
    )
    .unwrap();
    assert!(merges > 0);
    let retained = wires(&lab, 0, scope);

    let retry = pass_at(&lab.paths[0], now).unwrap();
    assert!(
        retry.writer,
        "a merge must not freeze the requester's writer"
    );
    assert_eq!(
        retry.posted + retry.answered + retry.offered + retry.swept,
        0
    );
    assert_eq!(wires(&lab, 0, scope), retained);
    receive(&lab, 1, scope, &retained);
    let peer = observe(&lab.paths[1]).unwrap();
    assert!(peer.book.inert.is_empty());
    let requests = |book: &Book| {
        book.threads
            .iter()
            .map(|(id, thread)| (*id, (thread.operation, thread.requester, thread.want.key())))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(requests(&peer.book), requests(&after.book));
    assert_eq!(wires(&lab, 1, scope), retained);
}

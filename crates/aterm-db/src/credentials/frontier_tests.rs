// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Large credential queues retain causal merges without inventing protocol actions.

use super::*;
use crate::table::tests::frontier::{receive, wide_lab, wires};
use astream_wire::sync::MAX_PARENTS;

#[test]
fn a_wide_credential_queue_keeps_requests_and_retries_without_phantom_actions() {
    let writers = MAX_PARENTS + 1;
    let lab = wide_lab(writers);
    let now = now_ms();
    for (writer, paths) in lab.paths.iter().enumerate() {
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
    let incoming = (1..writers)
        .flat_map(|writer| wires(&lab, writer, scope))
        .collect::<Vec<_>>();
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

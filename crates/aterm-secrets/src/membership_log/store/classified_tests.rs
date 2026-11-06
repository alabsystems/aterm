// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`Store::ingest_classified`]'s per-record verdicts, and the [`Written`] hook's dispatch.

use super::*;
use crate::age::Identity;
use crate::commons::binding::Request;
use crate::membership_log::{Entrant, Evidence, MemberId};

const NETWORK: [u8; 16] = [0x11; 16];
const OTHER_NETWORK: [u8; 16] = [0x33; 16];
const AUTHORITY: [u8; 32] = [0x22; 32];

fn entrant(tag: u8, network: [u8; 16]) -> ([u8; 32], [u8; 16], MemberId, Entrant) {
    let writer = [tag; 32];
    let machine = [tag.wrapping_add(0x20); 16];
    let incarnation = [tag; 16];
    let identity = Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
    let request = Request::create(network, AUTHORITY, machine, incarnation, &identity, &writer)
        .expect("request");
    let member = request.binding().actor();
    let entrant = Entrant {
        request,
        node_key: [tag.wrapping_add(0x80); 32],
        name: format!("mac-{tag}"),
        label: format!("m{tag}"),
    };
    (writer, incarnation, member, entrant)
}

/// One network's records: its genesis (founding m1), a record of m1's, m1's admission of m2
/// (m2's seat), and a record of m2's whose parent is that admission.
pub(crate) struct Chain {
    pub(crate) genesis: Record,
    pub(crate) first_by_m1: Record,
    pub(crate) admit: Record,
    pub(crate) by_m2: Record,
}

pub(crate) fn chain() -> Chain {
    let (w1, inc1, m1, e1) = entrant(1, NETWORK);
    let (w2, inc2, m2, e2) = entrant(2, NETWORK);
    let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
    let mut author = Log::new(genesis.clone()).expect("log");
    let first_by_m1 = author
        .draft(
            &m1,
            1_001,
            Body::Name {
                name: "m1-renamed".to_owned(),
            },
        )
        .sign(&w1, inc1)
        .expect("first by m1");
    author.insert(first_by_m1.clone()).expect("hold first");
    let admit = author
        .draft(
            &m1,
            1_002,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit");
    author.insert(admit.clone()).expect("hold admit");
    let by_m2 = author
        .draft(
            &m2,
            1_003,
            Body::Name {
                name: "m2-renamed".to_owned(),
            },
        )
        .sign(&w2, inc2)
        .expect("by m2");
    Chain {
        genesis,
        first_by_m1,
        admit,
        by_m2,
    }
}

fn verdict_of(verdicts: &[(RecordId, MemberVerdict)], id: &RecordId) -> MemberVerdict {
    verdicts
        .iter()
        .find(|(v, _)| v == id)
        .map(|(_, verdict)| verdict.clone())
        .expect("a verdict per offered record")
}

#[test]
fn a_held_record_is_applied_and_one_missing_a_parent_waits() {
    let c = chain();
    let mut store = Store::for_vault("team").expect("store");
    let verdicts = store.ingest_classified(vec![c.genesis.clone(), c.first_by_m1.clone()]);
    assert_eq!(
        verdicts,
        vec![
            (*c.genesis.id(), MemberVerdict::Applied),
            (*c.first_by_m1.id(), MemberVerdict::Applied),
        ]
    );
    // Held before: still applied.
    let again = store.ingest_classified(vec![c.first_by_m1.clone()]);
    assert_eq!(again, vec![(*c.first_by_m1.id(), MemberVerdict::Applied)]);

    // `by_m2`'s parent (`admit`) is missing, and its signer holds no seat in this view: the
    // store refuses it, and it waits.
    let verdicts = store.ingest_classified(vec![c.by_m2.clone()]);
    assert!(
        matches!(
            verdict_of(&verdicts, c.by_m2.id()),
            MemberVerdict::Waiting(_)
        ),
        "{verdicts:?}"
    );
    assert!(!store.contains(c.by_m2.id()));
}

#[test]
fn a_record_waiting_on_its_signers_seat_applies_once_the_seat_lands() {
    let c = chain();
    let mut store = Store::for_vault("team").expect("store");
    store.ingest_classified(vec![c.genesis.clone(), c.first_by_m1.clone()]);
    let first = store.ingest_classified(vec![c.by_m2.clone()]);
    assert!(
        matches!(verdict_of(&first, c.by_m2.id()), MemberVerdict::Waiting(_)),
        "{first:?}"
    );
    let seat = store.ingest_classified(vec![c.admit.clone()]);
    assert_eq!(seat, vec![(*c.admit.id(), MemberVerdict::Applied)]);
    let again = store.ingest_classified(vec![c.by_m2.clone()]);
    assert_eq!(again, vec![(*c.by_m2.id(), MemberVerdict::Applied)]);
}

#[test]
fn a_record_of_another_network_is_refused_for_good() {
    let c = chain();
    let (_, _, _, foreign_founder) = entrant(5, OTHER_NETWORK);
    let foreign = Record::found(&[8; 32], vec![foreign_founder], 1_000).expect("foreign genesis");
    let mut store = Store::for_vault("team").expect("store");
    store.ingest_classified(vec![c.genesis.clone()]);
    let verdicts = store.ingest_classified(vec![foreign.clone()]);
    assert!(
        matches!(
            verdict_of(&verdicts, foreign.id()),
            MemberVerdict::Refused(_)
        ),
        "{verdicts:?}"
    );
}

#[test]
fn a_refused_record_whose_parents_are_held_is_refused_for_good() {
    // `by_m2` re-signed by a key that holds no seat, its parent held: judged on a causal past
    // that never changes.
    let c = chain();
    let mut store = Store::for_vault("team").expect("store");
    store.ingest_classified(vec![
        c.genesis.clone(),
        c.first_by_m1.clone(),
        c.admit.clone(),
    ]);
    let (w9, inc9, m9, _) = entrant(9, NETWORK);
    let unseated = store
        .log()
        .expect("log")
        .draft(
            &m9,
            1_010,
            Body::Name {
                name: "nobody".to_owned(),
            },
        )
        .sign(&w9, inc9)
        .expect("unseated");
    let verdicts = store.ingest_classified(vec![unseated.clone()]);
    assert!(
        matches!(
            verdict_of(&verdicts, unseated.id()),
            MemberVerdict::Refused(_)
        ),
        "{verdicts:?}"
    );
}

#[test]
fn written_reaches_this_threads_override_and_not_once_the_call_is_removed() {
    let calls = std::rc::Rc::new(std::cell::Cell::new(0_usize));
    let seen = calls.clone();
    let store = Store::default();
    let dir = std::path::Path::new("/nowhere");
    with_written(
        move |_| seen.set(seen.get() + 1),
        || {
            written(&Written {
                paths_dir: dir,
                store: &store,
            });
            // MUTANT: the call removed.
            without_written_call(|| {
                written(&Written {
                    paths_dir: dir,
                    store: &store,
                });
            });
        },
    );
    assert_eq!(calls.get(), 1);
    // Out of the override, this thread's writes reach no test hook.
    written(&Written {
        paths_dir: dir,
        store: &store,
    });
    assert_eq!(calls.get(), 1);
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The catalog policy a replay judges once per open is the one each record was judged by before
//! (P1), and it binds the catalog's records alone (P2).

use super::*;
use crate::{
    Error, Result,
    catalog::{MAX_PAYLOAD, RECORDS_PER_CREATOR},
    table::store::{ANCHOR_LEN, CLAIM_LEN, Capacity, operation_policy, operation_policy_in},
};
use astream_wire::sync::Header;
use aterm_secrets::commons::journal::Trust;
use aterm_uds::private_file;

/// `operation_policy` as it was before the open judged the catalog once: the oracle, frozen.
fn reference_operation_policy(scope: Scope, header: &Header, payload: usize) -> Result<()> {
    if scope == crate::catalog::scope(scope.trust())
        && (payload > crate::catalog::MAX_PAYLOAD
            || header.sequence > crate::catalog::RECORDS_PER_CREATOR as u64
            || header.parents.iter().copied().ne(header.previous))
    {
        return Err(Error::new(
            "catalog operation exceeds its schema or causal bounds",
        ));
    }
    Ok(())
}

/// P1: over the catalog's scope and an application table's, an operation within the catalog's
/// bounds (at each bound), one whose payload is over `MAX_PAYLOAD`, one whose sequence is over
/// `RECORDS_PER_CREATOR` and one whose parents are not its previous record alone, the policy the
/// open asks (`operation_policy_in` of whether the scope is the catalog) and the one ingest asks
/// (`operation_policy`) answer as the frozen oracle does, which refuses the catalog's three.
#[test]
fn the_catalog_policy_judged_once_answers_as_the_policy_judged_per_record() {
    let trust = Trust {
        network: [1; 16],
        authority: [2; 32],
        genesis: [3; 32],
    };
    let catalog = crate::catalog::scope(trust);
    let application = Scope::new(trust, [0x81; 16], 1).unwrap();
    assert_ne!(application, catalog);
    let header = |sequence: u64, parents: &[u8]| Header {
        id: [9; 32],
        actor: [4; 48],
        sequence,
        previous: Some([5; 32]),
        parents: parents.iter().map(|p| [*p; 32]).collect(),
    };
    let bound = RECORDS_PER_CREATOR as u64;
    let cases = [
        ("within", header(bound, &[5]), MAX_PAYLOAD),
        ("payload", header(2, &[5]), MAX_PAYLOAD + 1),
        ("sequence", header(bound + 1, &[5]), 1),
        ("parents", header(2, &[5, 6]), 1),
    ];
    for (scope, refused) in [
        (catalog, [false, true, true, true]),
        (application, [false; 4]),
    ] {
        for ((case, header, payload), refused) in cases.iter().zip(refused) {
            let oracle = reference_operation_policy(scope, header, *payload).map_err(|e| e.0);
            assert_eq!(oracle.is_err(), refused, "{case}: the oracle");
            let once = scope == crate::catalog::scope(scope.trust());
            assert_eq!(
                operation_policy_in(once, header, *payload).map_err(|e| e.0),
                oracle,
                "{case}: judged once"
            );
            assert_eq!(
                operation_policy(scope, header, *payload).map_err(|e| e.0),
                oracle,
                "{case}: judged per record"
            );
        }
    }
}

/// P2 (i): an application table whose one writer holds more records than the catalog allows a
/// creator opens as an observer, its writer's records all held.
#[test]
fn an_application_table_past_the_catalogs_creator_bound_opens() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = lab.bootstrap(0);
    let dir = lab._home.path().join("long");
    let scope = {
        let session = Session::open(&a).unwrap();
        let mut store = Store::create(&session, &dir).unwrap();
        for n in 0..=RECORDS_PER_CREATOR {
            store
                .commit_catalog(&session, format!("row {n}").as_bytes())
                .unwrap();
        }
        store.scope()
    };
    let observer = Session::observe(&a).unwrap();
    let store = Store::observe(&observer, &dir, scope).unwrap();
    assert_eq!(
        store.retained_by(&observer, observer.actor()).unwrap(),
        RECORDS_PER_CREATOR + 1
    );
}

/// P2 (ii): a catalog journal whose one unwitnessed tail record is past the catalog's creator
/// bound is refused by the writing open that would adopt it, by the catalog policy.
#[test]
fn a_catalog_tail_past_the_creator_bound_is_refused_by_the_writing_open() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let scope = crate::catalog::scope(a.trust());
    let dir = lab._home.path().join("catalog");
    let (claim_dir, claim_name) = {
        let store = Store::join(&session, &dir, scope).unwrap();
        (store.claim_dir.clone(), store.claim_name.clone())
    };
    let claim = private_file::read_private(&claim_dir, &claim_name, CLAIM_LEN).unwrap();
    let beyond = session
        .seal(
            scope,
            Causal {
                sequence: RECORDS_PER_CREATOR as u64 + 1,
                previous: Some([7; 32]),
                parents: &[[7; 32]],
            },
            b"{}",
        )
        .unwrap();
    {
        let mut log = Log::open(
            &dir,
            "table",
            &claim[..ANCHOR_LEN],
            true,
            Capacity::default().journal(),
        )
        .unwrap();
        let mut body = aterm_time::unix_millis().unwrap().to_le_bytes().to_vec();
        body.extend_from_slice(&beyond);
        log.append(SUBJECT, body).unwrap();
    }
    let refused = Store::open(&session, &dir, scope).err().unwrap();
    assert_eq!(
        refused.0,
        "catalog operation exceeds its schema or causal bounds"
    );
}

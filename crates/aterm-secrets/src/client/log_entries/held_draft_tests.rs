// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::binding::Request;
use crate::crossing::fixture::LEGACY;

fn member(tag: u8) -> ([u8; 32], [u8; 16], MemberId, Entrant) {
    let seed = [tag; 32];
    let incarnation = [tag; 16];
    let identity = Identity::from_secret_bytes([tag + 64; 32]);
    let request = Request::create(
        [17; 16],
        [34; 32],
        [tag + 32; 16],
        incarnation,
        &identity,
        &seed,
    )
    .unwrap();
    let actor = request.binding().actor();
    (
        seed,
        incarnation,
        actor,
        Entrant {
            request,
            node_key: [tag + 128; 32],
            name: format!("m{tag}"),
            label: format!("m{tag}"),
        },
    )
}

fn name(log: &mut Log, actor: &MemberId, seed: &[u8; 32], incarnation: [u8; 16]) -> Record {
    let record = log
        .draft(
            actor,
            2_000,
            Body::Name {
                name: "test-member".into(),
            },
        )
        .sign(seed, incarnation)
        .unwrap();
    log.insert(record.clone()).unwrap();
    record
}

#[test]
fn accepted_publication_keeps_its_whole_draft_or_preserves_the_recovery_file() {
    LEGACY.within(accepted_publication_at_a_full_held_store);
}

fn accepted_publication_at_a_full_held_store() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (seed, incarnation, actor, founder) = member(1);
    let (peer_seed, peer_incarnation, peer, peer_founder) = member(2);
    let mut base =
        Log::new(Record::found(&[7; 32], vec![founder, peer_founder], 1_000).unwrap()).unwrap();
    while base.held_count() < LEGACY.held - 2 {
        name(&mut base, &actor, &seed, incarnation);
    }
    let mut after_draft = base.clone();
    let drafted = Drafted {
        records: vec![
            name(&mut after_draft, &actor, &seed, incarnation),
            name(&mut after_draft, &actor, &seed, incarnation),
        ],
        heads: after_draft.heads(),
    };
    let incoming = name(&mut base, &peer, &peer_seed, peer_incarnation);
    assert_eq!(base.held_count(), LEGACY.held - 1);

    // An ordinary publish has already been accepted. Concurrent peer ingress
    // consumed one of the two slots its held draft needs before local retention.
    let mut outcomes = Vec::new();
    for bounded in [false, true] {
        let paths =
            Paths::for_home(
                &root
                    .path()
                    .join(if bounded { "bounded" } else { "ordinary" }),
            );
        super::super::save_records(&paths, &encode_records(base.records())).unwrap();
        hold_drafted(&paths, &drafted).unwrap();
        let before = std::fs::read(paths.records_file()).unwrap();
        let held = std::fs::read(held_draft_file(&paths)).unwrap();
        let deadline = bounded.then(|| Instant::now() + aterm_spec::wait::MUST_HAPPEN);
        let result = keep_drafted_until(&paths, &drafted, &[], deadline, None);
        if result.is_ok() {
            // This is the publication caller's success path: it may discard
            // recovery only after every record was made durably complete.
            forget_held_draft(&paths);
        }
        let store = super::super::load_records(&paths).unwrap();
        let retained = drafted
            .records
            .iter()
            .filter(|record| store.holds(record.id()))
            .count();
        assert!(store.holds(incoming.id()));
        outcomes.push((
            bounded,
            result.is_err(),
            std::fs::read(paths.records_file()).unwrap() == before,
            std::fs::read(held_draft_file(&paths)).ok() == Some(held),
            retained,
        ));
    }
    assert_eq!(
        outcomes,
        [(false, true, true, true, 0), (true, true, true, true, 0)],
        "retention must refuse an incomplete draft without changing the store or discarding recovery"
    );
}

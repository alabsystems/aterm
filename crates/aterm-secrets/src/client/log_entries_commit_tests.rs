// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::binding::Request;
use crate::crossing::fixture::LEGACY;

fn entrant(tag: u8, label: &str) -> ([u8; 32], [u8; 16], MemberId, Entrant) {
    let writer = [tag; 32];
    let incarnation = [tag; 16];
    let identity = Identity::from_secret_bytes([tag + 64; 32]);
    let request = Request::create(
        [17; 16],
        [34; 32],
        [tag + 32; 16],
        incarnation,
        &identity,
        &writer,
    )
    .unwrap();
    let actor = request.binding().actor();
    (
        writer,
        incarnation,
        actor,
        Entrant {
            request,
            node_key: [tag + 128; 32],
            name: label.into(),
            label: label.into(),
        },
    )
}

fn append_name(
    log: &mut Log,
    actor: &MemberId,
    writer: &[u8; 32],
    incarnation: [u8; 16],
) -> Record {
    let record = log
        .draft(
            actor,
            2_000,
            Body::Name {
                name: "test-member".into(),
            },
        )
        .sign(writer, incarnation)
        .unwrap();
    log.insert(record.clone()).unwrap();
    record
}

#[test]
fn concurrent_ingress_cannot_commit_only_a_prefix_of_an_authored_batch() {
    LEGACY.within(concurrent_ingress_at_a_full_held_store);
}

fn concurrent_ingress_at_a_full_held_store() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let (writer, incarnation, actor, first) = entrant(1, "m1");
    let (peer_writer, peer_incarnation, peer, second) = entrant(2, "m2");
    let mut base = Log::new(Record::found(&[7; 32], vec![first, second], 1_000).unwrap()).unwrap();
    while base.held_count() < LEGACY.held - 8 {
        append_name(&mut base, &actor, &writer, incarnation);
    }
    super::super::save_records(&paths, &encode_records(base.records())).unwrap();

    let mut attempts = 0;
    let mut authored_ids = Vec::new();
    let mut peer_ids = Vec::new();
    let result = author_and_commit_records(&paths, |snapshot| {
        attempts += 1;
        let mut draft = snapshot.log().unwrap().clone();
        let first = append_name(&mut draft, &actor, &writer, incarnation);
        let second = append_name(&mut draft, &actor, &writer, incarnation);
        authored_ids = vec![*first.id(), *second.id()];

        // Peer ingress does not take the local author lock. Advance the actual
        // store after each draft, including the last allowed re-draft. Both
        // authored records fit the snapshot; only one fits that final store.
        let mut incoming = snapshot.log().unwrap().clone();
        let incoming: Vec<_> = (0..if attempts < 4 { 2 } else { 1 })
            .map(|_| append_name(&mut incoming, &peer, &peer_writer, peer_incarnation))
            .collect();
        peer_ids.extend(incoming.iter().map(|record| *record.id()));
        super::super::save_records(&paths, &encode_records(&incoming)).unwrap();
        Ok((vec![first, second], ()))
    });
    assert_eq!(attempts, 4);
    let after = super::super::load_records(&paths).unwrap();
    let retained = authored_ids.iter().filter(|id| after.contains(id)).count();
    assert!(
        result.is_err(),
        "reported a complete authored batch while retaining {retained} of 2 records"
    );
    assert_eq!(
        retained, 0,
        "a refused authored batch must not publish a prefix"
    );
    assert_eq!(after.len(), LEGACY.held - 1);
    assert!(
        peer_ids.iter().all(|id| after.contains(id)),
        "peer ingress remains durable"
    );

    // Refusing the two-record batch must not consume the last available slot.
    let (records, complete, ()) = author_and_commit_records(&paths, |snapshot| {
        let record = snapshot
            .log()
            .unwrap()
            .draft(
                &actor,
                3_000,
                Body::Name {
                    name: "fits-last-slot".into(),
                },
            )
            .sign(&writer, incarnation)
            .unwrap();
        Ok((vec![record], ()))
    })
    .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(complete.len(), LEGACY.held);
    assert!(complete.holds(records[0].id()));

    // An already-held batch is a complete idempotent result even at capacity.
    let before = std::fs::read(paths.records_file()).unwrap();
    let (again, _, ()) = author_and_commit_records(&paths, |_| Ok((records.clone(), ()))).unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(std::fs::read(paths.records_file()).unwrap(), before);
}

#[test]
fn an_admin_publish_that_cannot_fit_its_log_refuses_without_publishing() {
    LEGACY.within(admin_publication_at_a_full_held_store);
}

fn admin_publication_at_a_full_held_store() {
    let mut outcomes = Vec::new();
    for stamped in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("m1"));
        let source = root.path().join("vault");
        super::super::create(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
        let admin = super::super::admin_cached(&paths).unwrap();
        let mut doc = admin.published_doc.clone();
        doc.set_entry(&Audience::Everyone, "capacity-entry", b"before")
            .unwrap();
        super::super::admin_publish(&paths, &admin, &doc).unwrap();
        super::super::seed_vault_genesis(&paths, &root.path().join("vault.genesis")).unwrap();
        if stamped {
            let admin = super::super::admin_cached(&paths).unwrap();
            super::super::admin_publish(&paths, &admin, &admin.doc).unwrap();
        }

        let expected_base = if stamped {
            LogBase::Covers
        } else {
            LogBase::Unstamped
        };
        let admin = super::super::admin_cached(&paths).unwrap();
        assert_eq!(admin.log_base, expected_base);
        let identity = super::super::read_identity(&paths).unwrap().unwrap();
        let owner = crate::commons::bootstrap::Bootstrap::required(&paths).unwrap();
        assert!(
            trusted_commons_log(&paths, owner.trust())
                .unwrap()
                .is_some()
        );
        let request = owner.request().unwrap();
        let mut store = super::super::load_records(&paths).unwrap();
        assert!(store.log().unwrap().has_commons_epochs());
        assert!(
            store
                .log()
                .unwrap()
                .membership()
                .contains(&request.binding().actor())
        );
        owner.device().with_seed(|seed| {
            while store.log().unwrap().held_count() < LEGACY.held {
                let record = store
                    .log()
                    .unwrap()
                    .draft(
                        &request.binding().actor(),
                        2_000,
                        Body::Name { name: "m1".into() },
                    )
                    .sign(seed, *request.binding().incarnation())
                    .unwrap();
                assert!(store.insert(record).unwrap());
            }
        });
        fsx::write_atomic(&paths.records_file(), &store.encode(), 0o600).unwrap();
        let admin = super::super::admin_cached(&paths).unwrap();
        assert_eq!(admin.log_base, expected_base);
        let mut wanted = admin.published_doc.clone();
        wanted
            .set_entry(&Audience::Everyone, "capacity-entry", b"after")
            .unwrap();
        assert!(
            draft_document_diff(
                &paths,
                &admin.config,
                &envelope::signer_public(&admin.seed).unwrap(),
                &admin.handoff,
                &identity,
                &wanted,
                Base {
                    doc: stamped.then_some(&admin.published_doc),
                    resolve: Some(&admin.resolve),
                    readmit: None,
                },
            )
            .is_err(),
            "the actual log draft must reach the capacity refusal"
        );
        let before_records = std::fs::read(paths.records_file()).unwrap();
        let before_source = std::fs::read(&source).unwrap();

        let result = super::super::admin_publish(&paths, &admin, &wanted);

        outcomes.push((
            expected_base,
            result.is_err(),
            std::fs::read(paths.records_file()).unwrap() == before_records,
            std::fs::read(&source).unwrap() == before_source,
        ));
    }
    assert_eq!(
        outcomes,
        vec![
            (LogBase::Unstamped, true, true, true),
            (LogBase::Covers, true, true, true),
        ],
        "each refused draft must leave the log and signed source unchanged"
    );
}

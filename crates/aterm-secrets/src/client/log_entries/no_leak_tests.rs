// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One-mesh proofs P1b (`aterm-mesh-core`'s `revocation_no_leak_island.rs`): a dropped section's
//! tombstones are a `Put` step of the no-leak World, sealed as every other write is.

use super::super::log_entries_tests::{member_key, two_macs};
use super::*;

/// m22 and m7 share the host list `[m22 m7]`. m22 takes m7 out (a `Remove`, no rotation yet) and
/// seats m9, and the vault text then names the list `[m22]`, m7 having left `[machines]`: the next
/// publish tombstones the `[m22 m7]` entries. The list's newest epoch still wraps m7, so it is due
/// ([`Log::head_due`]), and a `Delete` sealed under it would tell m7 what the log did after it
/// left. The tombstones go under a fresh epoch that wraps no Mac the log removed, and only the
/// Macs the list's own epochs carry: m9, a seat the list never named, gets no `Wrap`.
///
/// NEGATIVE CONTROL: `ATERM_TEST_NEGATIVE_CONTROL=tombstone-ungated` seals them under the first
/// newest epoch m22 holds a key of, as before P1b, and the asserts that they avoid it fail.
#[test]
fn a_host_lists_tombstones_after_a_remove_are_sealed_where_no_removed_mac_reads_them() {
    let ring = Audience::Hosts(vec!["m22".to_owned(), "m7".to_owned()]);
    let macs = two_macs(&[
        (Audience::Everyone, "shared", b"everyone-v1".as_slice()),
        (ring.clone(), "ring-cred", b"ring-v1".as_slice()),
    ]);
    let m22 = &macs.m22;
    {
        let admin = admin_open(m22).unwrap();
        admin_publish(m22, &admin, &admin.doc.clone()).expect("a stamped version");
    }
    let m22_id = read_identity(m22).unwrap().unwrap();
    let m7_id = read_identity(&macs.m7).unwrap().unwrap();
    let m7_recipient = m7_id.recipient();
    let ring_epochs = EpochAudience::from(&ring);
    let log = load_records(m22).unwrap().log().unwrap().clone();
    let opened = log.open_audience_entries(&m22_id).unwrap().unwrap();
    assert!(opened.by_audience.contains_key(&ring), "m22 opens [m22 m7]");
    let newest = opened.maximal_epochs[&ring_epochs].clone();
    assert!(
        newest.iter().all(|epoch| opened
            .wrapped_recipients
            .get(epoch)
            .is_some_and(|wrapped| wrapped.contains(&m7_recipient))),
        "the list's newest epochs wrap m7"
    );

    // m22 takes m7 out, with no rotation.
    let (m7_actor, entry) = log
        .membership()
        .members()
        .iter()
        .find(|(_, member)| member.binding.recipient() == m7_recipient)
        .map(|(actor, member)| (*actor, member.entry))
        .expect("m7's seat");
    let cutoff = log
        .records()
        .filter(|record| record.actor() == &m7_actor)
        .max_by_key(|record| record.sequence())
        .map_or(entry, |record| *record.id());
    let (bootstrap, actor, incarnation) = member_key(m22);
    let remove = bootstrap
        .device()
        .with_seed(|seed| {
            log.draft(
                &actor,
                1_000,
                Body::Remove {
                    member: m7_actor,
                    cutoff,
                },
            )
            .sign(seed, incarnation)
        })
        .unwrap();
    // m22 seats m9 after the list's epoch: a current seat the list never named.
    let trust = bootstrap.trust();
    let m9 = Identity::from_secret_bytes([0x99; 32]);
    let m9_recipient = m9.recipient();
    let admit = {
        let mut held = log.clone();
        held.insert(remove.clone()).unwrap();
        let entrant = crate::membership_log::Entrant {
            request: crate::commons::binding::Request::create(
                trust.network,
                trust.authority,
                [0x99; 16],
                [0x99; 16],
                &m9,
                &[0x99; 32],
            )
            .unwrap(),
            node_key: [0x99; 32],
            name: "m9".to_owned(),
            label: "m9".to_owned(),
        };
        bootstrap
            .device()
            .with_seed(|seed| {
                held.draft(
                    &actor,
                    1_001,
                    Body::Admit {
                        entrant,
                        evidence: crate::membership_log::Evidence::Phrase,
                    },
                )
                .sign(seed, incarnation)
            })
            .unwrap()
    };
    save_records(m22, &encode_records(&[remove, admit])).unwrap();
    let log = load_records(m22).unwrap().log().unwrap().clone();
    assert!(
        log.current_recipients(&EpochAudience::Everyone)
            .contains(&m9_recipient)
    );
    assert!(log.recipient_is_removed(&m7_recipient));
    let removed = log.removed_recipients();
    assert!(
        newest.iter().any(|epoch| log.head_due(
            epoch,
            opened.wrapped_recipients.get(epoch),
            &removed
        )),
        "the list's newest epoch is due"
    );

    // The text names the list `[m22]` now, so the `[m22 m7]` section is gone from it.
    let admin = admin_open(m22).unwrap();
    assert!(admin.doc.entry(&ring, "ring-cred").is_none());
    let hosts = Audience::Hosts(vec!["m22".to_owned()]);
    assert!(admin.doc.entry(&hosts, "ring-cred").is_some());
    let cfg = config(m22).unwrap().unwrap();
    let cached = envelope::parse(&std::fs::read(m22.cache_file()).unwrap()).unwrap();
    let drafted = draft_document_diff(
        m22,
        &cfg,
        &cached.signer,
        &cached.handoff,
        &m22_id,
        &admin.doc,
        Base {
            doc: Some(&admin.doc),
            resolve: None,
            readmit: None,
        },
    )
    .unwrap()
    .expect("m22 writes the log");
    let tombstones: Vec<RecordId> = drafted
        .records
        .iter()
        .filter_map(|record| match record.body() {
            Body::Delete { epoch, .. } => Some(*epoch),
            _ => None,
        })
        .collect();
    assert!(
        !tombstones.is_empty(),
        "m22 tombstones the `[m22 m7]` entries"
    );
    let mut after = log;
    for record in &drafted.records {
        after.insert(record.clone()).unwrap();
    }
    for epoch in &tombstones {
        assert!(
            !newest.contains(epoch),
            "a tombstone sealed under the due epoch"
        );
        assert!(
            !after.records().any(|record| matches!(
                record.body(),
                Body::Wrap { epoch: wrapped, recipient, .. } if wrapped == epoch && *recipient == m7_recipient
            )),
            "a tombstone's epoch wraps m7, taken out"
        );
        assert!(
            !after.records().any(|record| matches!(
                record.body(),
                Body::Wrap { epoch: wrapped, recipient, .. } if wrapped == epoch && *recipient == m9_recipient
            )),
            "a tombstone's epoch wraps m9, a seat the list never named"
        );
    }

    macs.server.stop();
    drop(macs.root);
}

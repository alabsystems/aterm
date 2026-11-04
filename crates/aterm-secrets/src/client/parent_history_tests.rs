// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

/// Advance authenticated envelope history without changing its lockers. This
/// represents many root publications while the second store is disconnected.
fn advance_history(source: &Source, seed: &[u8; 32], mut bytes: Vec<u8>) -> Vec<u8> {
    for _ in 0..=envelope::MAX_PARENTS {
        let previous = envelope::parse(&bytes).unwrap();
        bytes = envelope::sign(
            &Unsigned {
                name: previous.name.clone(),
                seq: envelope::next_seq(previous.seq).unwrap(),
                time: previous.time,
                handoff: previous.handoff.clone(),
                parents: extend_parent_spine(&[previous.digest()], &[previous.parents.as_slice()]),
                lockers: previous.lockers.clone(),
            },
            seed,
        )
        .unwrap();
        transport::publish_until(source, &bytes, Some(deadline())).unwrap();
    }
    bytes
}

#[test]
fn an_admin_reconciles_beyond_the_parent_window_before_a_reader_catches_up() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"before")]);
    let second = log_entries_tests::start_loopback_server(&lab.root.path().join("second"));
    let second_url = format!("http://{}/v1/alab", second.addr);
    let second_source = Source::Url(second_url.clone());
    let base = fsx::read_owned(&lab.m22.cache_file(), envelope::MAX_ENVELOPE)
        .unwrap()
        .unwrap();
    let old = envelope::parse(&base).unwrap();
    transport::publish(&second_source, &base).unwrap();

    // The returning reader uses its stale store as primary, with the current
    // store available as a peer. A higher sequence alone cannot prove ancestry.
    let mut reader_config = config(&lab.m7).unwrap().unwrap();
    reader_config.source = second_source.clone();
    save_config(&lab.m7, &reader_config).unwrap();
    sync(&lab.m7, None).unwrap();
    assert_eq!(load_state(&lab.m7).unwrap().digest, Some(old.digest()));

    let admin = admin_open(&lab.m22).unwrap();
    let current = advance_history(&admin.config.source, &admin.seed, base);
    let current = envelope::parse(&current).unwrap();
    assert_eq!(current.parents.len(), envelope::MAX_PARENTS);
    assert!(!current.parents.contains(&old.digest()));

    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"after long absence")
        .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    let newest = read_server_heads(&lab.m22, &[]).unwrap().remove(0);
    assert!(!newest.envelope.parents.contains(&old.digest()));

    let reader_before = load_state(&lab.m7).unwrap();
    bounded::sync_reader_with_candidates_until(&lab.m7, deadline(), std::slice::from_ref(&lab.url))
        .unwrap();
    // Applying new log records may report Applied even at the same envelope
    // sequence, so judge the retained head independently from the outcome tag.
    assert_eq!(load_state(&lab.m7).unwrap().digest, reader_before.digest);
    // The independent authenticated membership log can already deliver the
    // edit even though the envelope's bounded ancestry cannot advance yet.
    assert_eq!(
        std::fs::read(lab.m7.home.join(".secrets/shared")).unwrap(),
        b"after long absence"
    );

    // The normal admin pass names both verified heads in a new signed merge;
    // it must not force the newer envelope over the stale store's parent check.
    sync_with_candidates(&lab.m22, None, std::slice::from_ref(&second_url)).unwrap();
    let heads = read_server_heads(&lab.m22, std::slice::from_ref(&second_url)).unwrap();
    assert_eq!(heads.len(), 2);
    assert_eq!(heads[0].envelope.digest(), heads[1].envelope.digest());
    assert!(heads[0].envelope.parents.contains(&old.digest()));
    assert!(
        heads[0]
            .envelope
            .parents
            .contains(&newest.envelope.digest())
    );

    bounded::sync_reader_with_candidates_until(&lab.m7, deadline(), std::slice::from_ref(&lab.url))
        .unwrap();
    assert_eq!(
        load_state(&lab.m7).unwrap().digest,
        Some(heads[0].envelope.digest())
    );
    assert_eq!(
        std::fs::read(lab.m7.home.join(".secrets/shared")).unwrap(),
        b"after long absence"
    );
    second.stop();
    lab.server.stop();
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn until() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

#[test]
fn conditional_publish_binds_its_parent_and_regular_updates_keep_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let source = dir.path().join("team.vault");
    let paths = Paths::for_home(&dir.path().join("admin"));
    create(&paths, source.to_str().unwrap(), None, Some("admin")).unwrap();
    let before = admin_open(&paths).unwrap();
    let parent = before.opened_heads[0];
    let lagging = Source::Path(dir.path().join("lagging.vault"));
    transport::publish(
        &lagging,
        &fsx::read_owned(&source, envelope::MAX_ENVELOPE)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let mut doc = before.doc.clone();
    doc.set_entry(&Audience::Everyone, "test", b"conditional")
        .unwrap();
    let outcome = admin_publish_conditional_until(&paths, &before, &doc, until()).unwrap();
    assert_eq!(outcome.seq, before.seq + 1);
    let bytes = fsx::read_owned(&source, envelope::MAX_ENVELOPE)
        .unwrap()
        .unwrap();
    let parsed = envelope::parse(&bytes).unwrap();
    assert_eq!(parsed.parents, vec![parent]);
    let next_parent = parsed.digest();
    let next = admin_cached(&paths).unwrap();
    let mut next_doc = next.doc.clone();
    next_doc
        .set_entry(&Audience::Everyone, "test", b"ordinary")
        .unwrap();
    admin_publish(&paths, &next, &next_doc).unwrap();
    let bytes = fsx::read_owned(&source, envelope::MAX_ENVELOPE)
        .unwrap()
        .unwrap();
    let parents = envelope::parse(&bytes).unwrap().parents;
    assert_eq!(parents.first(), Some(&next_parent));
    assert!(parents.contains(&parent));
    transport::publish(&lagging, &bytes).expect("lagging store catches up across two versions");
}

#[test]
fn a_higher_sequence_does_not_license_a_stale_conditional_parent() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let source = dir.path().join("team.vault");
    let paths = Paths::for_home(&dir.path().join("admin"));
    create(&paths, source.to_str().unwrap(), None, Some("admin")).unwrap();
    let mut stale = admin_open(&paths).unwrap();
    let current = admin_open(&paths).unwrap();
    let mut doc = current.doc.clone();
    doc.set_entry(&Audience::Everyone, "other", b"retain me")
        .unwrap();
    admin_publish(&paths, &current, &doc).unwrap();
    // Model a higher-sequence peer fork: its counter alone must not authorize
    // replacement of a head absent from the envelope's signed parents.
    stale.seq += 10;
    let mut stale_doc = stale.doc.clone();
    stale_doc
        .set_entry(&Audience::Everyone, "test", b"candidate")
        .unwrap();
    let retained = fsx::read_owned(&source, envelope::MAX_ENVELOPE)
        .unwrap()
        .unwrap();
    assert!(admin_publish_conditional_until(&paths, &stale, &stale_doc, until()).is_err());
    assert_eq!(
        fsx::read_owned(&source, envelope::MAX_ENVELOPE)
            .unwrap()
            .unwrap()
            .as_slice(),
        retained.as_slice()
    );
    // Negative control: legacy sequence-only publication accepts this fork.
    admin_publish(&paths, &stale, &stale_doc).unwrap();
    assert_ne!(
        fsx::read_owned(&source, envelope::MAX_ENVELOPE)
            .unwrap()
            .unwrap()
            .as_slice(),
        retained.as_slice()
    );
}

#[test]
fn http_store_rejects_a_sibling_and_a_parentless_downgrade() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let dir = aterm_tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let server = crate::server::start(
        listener,
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: dir.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_secs(60),
            keep_awake: false,
        },
    )
    .unwrap();
    let url = format!("http://{}/v1/team", server.addr);
    let paths = Paths::for_home(&dir.path().join("admin"));
    create(&paths, &url, None, Some("admin")).unwrap();
    let mut sibling = admin_open(&paths).unwrap();
    let current = admin_open(&paths).unwrap();
    let mut doc = current.doc.clone();
    doc.set_entry(&Audience::Everyone, "test", b"winner")
        .unwrap();
    admin_publish_conditional_until(&paths, &current, &doc, until()).unwrap();
    let source = Source::Url(url);
    let read = || match transport::fetch(&source, None, Duration::from_secs(60)).unwrap() {
        Fetched::Body(bytes) => bytes,
        other => panic!("{other:?}"),
    };
    let retained = read();
    assert_eq!(
        envelope::parse(&retained).unwrap().parents,
        vec![current.opened_heads[0]]
    );
    sibling.seq += 10;
    let mut fork = sibling.doc.clone();
    fork.set_entry(&Audience::Everyone, "test", b"sibling")
        .unwrap();
    assert!(admin_publish_conditional_until(&paths, &sibling, &fork, until()).is_err());
    // A legacy parentless write cannot turn off protection at this store.
    assert!(admin_publish(&paths, &sibling, &fork).is_err());
    assert_eq!(read(), retained);
    let next = admin_open(&paths).unwrap();
    let mut doc = next.doc.clone();
    doc.set_entry(&Audience::Everyone, "test", b"successor")
        .unwrap();
    admin_publish(&paths, &next, &doc).unwrap();
    assert_eq!(
        envelope::parse(&read()).unwrap().parents.first(),
        Some(&envelope::parse(&retained).unwrap().digest())
    );
    let before_cut = admin_cached(&paths).unwrap();
    let mut doc = before_cut.doc.clone();
    doc.set_entry(&Audience::Everyone, "test", b"stored before lost response")
        .unwrap();
    CUT_AFTER_STORE.with(|cut| cut.set(true));
    assert!(admin_publish_conditional_until(&paths, &before_cut, &doc, until()).is_err());
    assert_eq!(admin_cached(&paths).unwrap().seq(), before_cut.seq());
    assert_eq!(envelope::parse(&read()).unwrap().seq, before_cut.seq() + 1);
    sync_reader_until(&paths, until()).unwrap();
    let recovered = admin_cached_until(&paths, until()).unwrap();
    assert_eq!(recovered.seq(), before_cut.seq() + 1);
    assert_eq!(
        recovered
            .published_entry(&Audience::Everyone, "test")
            .unwrap()
            .value
            .as_slice(),
        b"stored before lost response"
    );
    server.stop();
}

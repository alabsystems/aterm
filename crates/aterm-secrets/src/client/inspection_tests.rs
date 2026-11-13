// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{
    client::{self, Paths},
    text::{Audience, Document},
};

fn history(paths: &Paths) -> crate::Result<Option<super::MembershipHistoryInspection>> {
    super::inspect_membership_history_until(
        paths,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
}

#[test]
fn membership_history_is_readable_without_admin_or_current_commons_access() {
    use crate::client::log_entries_tests::{member_key, two_macs};
    use crate::membership_log::{EpochAudience, encode_records};
    let macs = two_macs(&[(Audience::Everyone, "private-entry", b"private-value")]);
    assert!(client::admin_cached(&macs.m7).is_err());
    let (bootstrap, actor, incarnation) = member_key(&macs.m22);
    let mut log = client::load_records(&macs.m22)
        .unwrap()
        .log()
        .unwrap()
        .clone();
    let identity = client::read_identity(&macs.m22).unwrap().unwrap();
    let generation = log
        .open_audience_entries(&identity)
        .unwrap()
        .unwrap()
        .max_generation[&EpochAudience::Commons]
        + 1;
    let records = bootstrap.device().with_seed(|seed| {
        let (epoch, key) = log
            .draft_epoch(&actor, 1000, EpochAudience::Commons, generation)
            .unwrap();
        let epoch = epoch.sign(seed, incarnation).unwrap();
        log.insert(epoch.clone()).unwrap();
        let wrap = log
            .draft_wrap(&actor, 1000, *epoch.id(), &key, identity.recipient())
            .unwrap()
            .sign(seed, incarnation)
            .unwrap();
        vec![epoch, wrap]
    });
    client::save_records(&macs.m7, &encode_records(&records)).unwrap();
    assert!(matches!(
        crate::commons::observe(&macs.m7).unwrap(),
        crate::commons::Observation::Unavailable(crate::commons::Unavailable::ControlExcluded)
    ));
    let files = || {
        [
            macs.m7.records_file(),
            macs.m7.identity_file(),
            macs.m7.state_file(),
            macs.m7.cache_file(),
        ]
        .map(|path| std::fs::read(path).unwrap())
    };
    let before = files();
    let store = client::load_records(&macs.m7).unwrap();
    let inspected = history(&macs.m7).unwrap().unwrap();
    assert_eq!(inspected.network, bootstrap.trust().network);
    assert_eq!(inspected.complete, store.log().unwrap().counts());
    assert_eq!(inspected.retained_records(), store.len());
    assert_eq!(inspected.bytes, store.encoded_len());
    assert_eq!(inspected.waiting, 0);
    assert_eq!(files(), before);
    assert!(!format!("{inspected:?}").contains("private-value"));
    macs.server.stop();
}

#[test]
fn membership_history_counts_waiting_records_against_available_slots() {
    use crate::client::log_entries_tests::{member_key, two_macs};
    use crate::membership_log::{Body, encode_records};
    let macs = two_macs(&[]);
    let before = history(&macs.m7).unwrap().unwrap();
    let (bootstrap, actor, incarnation) = member_key(&macs.m22);
    let mut log = client::load_records(&macs.m22)
        .unwrap()
        .log()
        .unwrap()
        .clone();
    let child = bootstrap.device().with_seed(|seed| {
        let parent = log
            .draft(
                &actor,
                1000,
                Body::Name {
                    name: "missing-parent".into(),
                },
            )
            .sign(seed, incarnation)
            .unwrap();
        log.insert(parent).unwrap();
        log.draft(
            &actor,
            1001,
            Body::Name {
                name: "waiting-child".into(),
            },
        )
        .sign(seed, incarnation)
        .unwrap()
    });
    client::save_records(&macs.m7, &encode_records(&[child])).unwrap();
    let after = history(&macs.m7).unwrap().unwrap();
    assert_eq!(after.complete, before.complete);
    assert_eq!(
        after.complete_inventory_sha256,
        before.complete_inventory_sha256
    );
    assert_eq!(after.waiting, 1);
    assert_eq!(after.retained_records(), before.retained_records() + 1);
    assert_eq!(after.remaining_records() + 1, before.remaining_records());
    macs.server.stop();
}

#[test]
fn membership_inventory_compares_sets_not_counts_or_arrival_order() {
    use crate::client::log_entries_tests::{member_key, two_macs};
    use crate::membership_log::Body;
    let macs = two_macs(&[]);
    let (first, first_actor, first_incarnation) = member_key(&macs.m22);
    let (second, second_actor, second_incarnation) = member_key(&macs.m7);
    let baseline = client::load_records(&macs.m22)
        .unwrap()
        .log()
        .unwrap()
        .clone();
    let a = first.device().with_seed(|seed| {
        baseline
            .draft(
                &first_actor,
                1000,
                Body::Name {
                    name: "left".into(),
                },
            )
            .sign(seed, first_incarnation)
            .unwrap()
    });
    let second_record = |name: &str| {
        second.device().with_seed(|seed| {
            baseline
                .draft(&second_actor, 1000, Body::Name { name: name.into() })
                .sign(seed, second_incarnation)
                .unwrap()
        })
    };
    let b = second_record("right");
    let replacement = second_record("different-right");
    let mut left = baseline.clone();
    left.insert(a.clone()).unwrap();
    left.insert(b.clone()).unwrap();
    let mut reordered = baseline.clone();
    reordered.insert(b.clone()).unwrap();
    reordered.insert(a.clone()).unwrap();
    assert_ne!(left.record_index(a.id()), reordered.record_index(a.id()));
    assert_eq!(
        super::membership_inventory(&left),
        super::membership_inventory(&reordered)
    );
    let mut different = baseline;
    different.insert(a).unwrap();
    different.insert(replacement).unwrap();
    assert_eq!(left.counts(), different.counts());
    assert_ne!(
        super::membership_inventory(&left),
        super::membership_inventory(&different)
    );
    macs.server.stop();
}

#[test]
fn membership_history_distinguishes_busy_absent_and_foreign_state_from_zero() {
    let absent = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&absent.path().join("absent"));
    assert!(history(&paths).unwrap().is_none());
    assert!(!paths.home.exists());
    let macs = client::log_entries_tests::two_macs(&[]);
    let held = client::StateLock::take(&macs.m7).unwrap();
    let error =
        super::inspect_membership_history_until(&macs.m7, std::time::Instant::now()).unwrap_err();
    assert_eq!(error.kind(), crate::ErrorKind::VaultBusy);
    drop(held);
    let foreign = client::log_entries_tests::fresh_two_macs(&[]);
    let wire = client::load_records(&foreign.m7).unwrap().encode();
    crate::fsx::write_atomic(&macs.m7.records_file(), &wire, 0o600).unwrap();
    assert!(
        history(&macs.m7).is_err(),
        "the retained root makes foreign replacement explicit damage"
    );
    assert_eq!(std::fs::read(macs.m7.records_file()).unwrap(), wire);
    macs.server.stop();
    foreign.server.stop();
}

#[test]
fn control_inspection_uses_published_state_and_exposes_no_payload() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("owner"));
    client::create(
        &paths,
        root.path().join("vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let mut admin = client::admin_cached(&paths).unwrap();
    let before = admin.inspect_commons_control().unwrap();
    assert!(before.entry_present && before.lifetime_bound);
    assert_eq!(before.audience, Some(Audience::Hosts(vec!["m1".into()])));
    assert_eq!(before.descriptor.unwrap().authority, before.authority);
    admin.doc = Document::new_vault(
        "draft",
        &client::machine_identity(&paths)
            .unwrap()
            .recipient()
            .to_bech32(),
    );
    admin
        .doc
        .set_entry(
            &Audience::Everyone,
            "private-payload",
            b"do-not-expose-this-value",
        )
        .unwrap();
    let after = admin.inspect_commons_control().unwrap();
    assert_eq!(before, after);
    assert!(!format!("{after:?}").contains("do-not-expose-this-value"));
}

#[test]
fn control_inspection_distinguishes_absent_and_opaque_without_initializing() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("owner"));
    client::test_create_unbound(
        &paths,
        root.path().join("vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let admin = client::admin_cached(&paths).unwrap();
    let absent = admin.inspect_commons_control().unwrap();
    assert!(!absent.entry_present && absent.descriptor.is_none());
    assert!(absent.audience.is_none());
    assert!(!paths.dir.join("commons").exists());
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Hosts(vec!["m1".into()]),
        crate::commons::ENTRY,
        b"opaque-legacy-control",
    )
    .unwrap();
    client::admin_publish(&paths, &admin, &doc).unwrap();
    let opaque = client::admin_cached(&paths)
        .unwrap()
        .inspect_commons_control()
        .unwrap();
    assert!(opaque.entry_present && opaque.descriptor.is_none());
    assert_eq!(opaque.audience, Some(Audience::Hosts(vec!["m1".into()])));
    assert!(!paths.dir.join("commons").exists());
}

#[test]
fn control_inspection_keeps_the_published_audience_before_the_log_overlay() {
    let macs = client::log_entries_tests::two_macs(&[]);
    let before = client::admin_cached(&macs.m22)
        .unwrap()
        .inspect_commons_control()
        .unwrap();
    assert_eq!(
        before.audience,
        Some(Audience::Hosts(vec!["m22".into(), "m7".into()]))
    );
    let published = std::fs::read(macs.m22.cache_file()).unwrap();
    let (_, peer, _) = client::log_entries_tests::member_key(&macs.m7);
    assert!(
        !client::remove::remove_actor(&macs.m22, peer)
            .unwrap()
            .is_empty()
    );
    for admin in [
        client::admin_cached(&macs.m22).unwrap(),
        client::admin_open(&macs.m22).unwrap(),
    ] {
        // Negative control: the editable baseline has already applied the log-only
        // removal, so reading it would misreport the audience in the published vault.
        assert_eq!(
            admin.published_doc.commons_audience(),
            Some(&Audience::Hosts(vec!["m22".into()]))
        );
        assert_ne!(
            admin.published_doc.commons_audience(),
            before.audience.as_ref()
        );
        assert_eq!(admin.inspect_commons_control().unwrap(), before);
    }
    assert_eq!(std::fs::read(macs.m22.cache_file()).unwrap(), published);
    macs.server.stop();
}

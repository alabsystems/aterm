// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Lab {
    _root: aterm_tempfile::TempDir,
    paths: Paths,
    source: PathBuf,
    /// What a machine joins by: the source and its signer.
    pinned: String,
}

fn lab(name: &str) -> Lab {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join(format!("{name}.vault"));
    let created = test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let admin = admin_open(&paths).unwrap();
    admin_keys::legacy::publish_legacy(&paths, &admin);
    Lab {
        pinned: format!("{}#{}", source.display(), created.signer),
        _root: root,
        paths,
        source,
    }
}

impl Lab {
    /// A machine `host` joins with its own identity, after `admit` puts it in the vault.
    fn machine(&self, host: &str, admit: impl FnOnce(&Admin, &mut Document, &str)) -> Paths {
        let machine = Paths::for_home(&self._root.path().join(host));
        let recipient = machine_identity(&machine).unwrap().recipient().to_bech32();
        let mut admin = admin_open(&self.paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine(host, &recipient).unwrap();
        admin.doc = doc.clone();
        admit(&admin, &mut doc, &recipient);
        join(
            &machine,
            &JoinOptions {
                source: self.pinned.clone(),
                host: Some(host.into()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        machine
    }

    fn admission(&self, host: &str) -> Option<AdmissionEvidence> {
        admin_open(&self.paths).unwrap().commons_admission(host)
    }

    fn audience(&self) -> Vec<String> {
        match admin_open(&self.paths).unwrap().doc.commons_audience() {
            Some(Audience::Hosts(hosts)) => hosts.clone(),
            _ => Vec::new(),
        }
    }
}

/// `owner` publishes `doc` with `hosts` as its control entry's audience.
fn share(owner: &Paths, admin: &Admin, mut doc: Document, hosts: &[&str]) {
    let audience = doc.commons_audience().unwrap().clone();
    let value = doc
        .entries()
        .into_iter()
        .find(|e| e.name == crate::commons::ENTRY)
        .unwrap()
        .value;
    doc.unset_entry(&audience, crate::commons::ENTRY).unwrap();
    let hosts = hosts.iter().map(|h| (*h).into()).collect();
    doc.set_entry(
        &Audience::Hosts(hosts),
        crate::commons::ENTRY,
        value.as_slice(),
    )
    .unwrap();
    admin_publish(owner, admin, &doc).unwrap();
}

#[test]
fn a_member_from_before_commons_enrolls_once_the_vault_is_founded() {
    let lab = lab("legacy-member");
    // m2 joined while the admin locker was at version 1, which records no decisions.
    let m2 = lab.machine("m2", |admin, _, _| {
        admin_keys::legacy::publish_legacy(&lab.paths, admin);
    });
    assert!(lab.admission("m2").is_none());
    let founded = admin_commons_init(&lab.paths).unwrap();
    assert_eq!(founded.eligible, 2);
    assert_eq!(founded.approved, ["m1", "m2"]);
    let seq = admin_open(&lab.paths).unwrap().seq();
    for host in ["m1", "m2"] {
        let evidence = lab.admission(host).unwrap();
        assert_eq!(
            (evidence.method, evidence.sequence),
            (AdmissionMethod::Manual, seq),
            "{host}: the founding publication approves every member it shares the root with"
        );
    }
    sync(&m2, None).unwrap();
    let owner = crate::commons::bootstrap::Bootstrap::required(&lab.paths).unwrap();
    let peer = crate::commons::bootstrap::Bootstrap::required(&m2).unwrap();
    let request = peer.request().unwrap();
    let (mut pending, challenge) = crate::commons::binding::Pending::issue(
        &request,
        owner.trust().network,
        owner.trust().authority,
    )
    .unwrap();
    let answer = peer.device().answer(&request, &challenge).unwrap();
    let mut journal =
        crate::commons::journal::Journal::open_existing(owner.journal_dir(), owner.trust())
            .unwrap();
    admin_cached(&lab.paths)
        .unwrap()
        .enroll_commons_member(&owner, &mut journal, pending.verify(&answer).unwrap())
        .unwrap();
}

/// Found Commons on `paths`' vault as an aterm that recorded no admission decisions did.
fn found_without_decisions(paths: &Paths) {
    let admin = admin_open(paths).unwrap();
    let descriptor = prepare(paths, &admin).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &audience(&admin),
        crate::commons::ENTRY,
        &descriptor.bytes(),
    )
    .unwrap();
    admin_publish(paths, &admin, &doc).unwrap();
}

#[test]
fn a_root_an_earlier_aterm_founded_approves_only_undecided_members_of_its_audience() {
    let lab = lab("vouch-retry");
    // m2 was in the vault when an earlier aterm founded its Commons.
    lab.machine("m2", |admin, doc, _| {
        admin_publish(&lab.paths, admin, doc).unwrap();
    });
    found_without_decisions(&lab.paths);
    assert!(lab.admission("m1").is_none() && lab.admission("m2").is_none());
    // m3 is let in as an open vault's approver lets a machine in.
    lab.machine("m3", |admin, doc, recipient| {
        publish_as(
            &lab.paths,
            admin,
            doc,
            false,
            &[(recipient.into(), AdmissionMethod::Open)],
            false,
        )
        .unwrap();
    });
    let open = lab.admission("m3").unwrap();
    // m4 comes in by a plain edit, outside the control entry's audience.
    lab.machine("m4", |admin, doc, _| {
        admin_publish(&lab.paths, admin, doc).unwrap();
    });
    assert_eq!(lab.audience(), ["m1", "m2", "m3"]);

    let retried = admin_commons_init(&lab.paths).unwrap();
    assert!(!retried.published, "the root is never replaced");
    assert_eq!(retried.approved, ["m1", "m2"]);
    let seq = admin_open(&lab.paths).unwrap().seq();
    for host in ["m1", "m2"] {
        let evidence = lab.admission(host).unwrap();
        assert_eq!(
            (evidence.method, evidence.sequence),
            (AdmissionMethod::Manual, seq)
        );
    }
    assert_eq!(
        lab.admission("m3"),
        Some(open),
        "a decision is never replaced"
    );
    assert!(lab.admission("m4").is_none(), "outside the audience");
    assert_eq!(lab.audience(), ["m1", "m2", "m3"], "and kept outside it");

    let published = std::fs::read(&lab.source).unwrap();
    assert!(admin_commons_init(&lab.paths).unwrap().approved.is_empty());
    assert!(
        std::fs::read(&lab.source).unwrap() == published,
        "no vault version is published"
    );
}

#[test]
fn a_root_this_build_founded_approves_no_machine_a_generic_edit_put_in_its_audience() {
    let lab = lab("vouch-generic");
    admin_commons_init(&lab.paths).unwrap();
    // m2 comes in by a plain edit that also names it in the control entry's audience.
    lab.machine("m2", |admin, doc, _| {
        share(&lab.paths, admin, doc.clone(), &["m1", "m2"]);
    });
    assert_eq!(lab.audience(), ["m1", "m2"]);
    let published = std::fs::read(&lab.source).unwrap();
    let retried = admin_commons_init(&lab.paths).unwrap();
    assert!(retried.approved.is_empty(), "{:?}", retried.approved);
    assert!(lab.admission("m2").is_none());
    assert!(
        std::fs::read(&lab.source).unwrap() == published,
        "no vault version is published"
    );
}

#[test]
fn a_run_that_finds_the_root_unready_records_no_approval() {
    let lab = lab("vouch-unready");
    let m2 = lab.machine("m2", |admin, doc, _| {
        admin_publish(&lab.paths, admin, doc).unwrap();
    });
    found_without_decisions(&lab.paths);
    // m2 is named an admin but never enrolled in the root, so its run cannot finish.
    let admin = admin_open(&lab.paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_admin("m2").unwrap();
    admin_publish(&lab.paths, &admin, &doc).unwrap();
    sync(&m2, None).unwrap();
    let published = std::fs::read(&lab.source).unwrap();
    assert!(admin_commons_init(&m2).is_err());
    assert!(
        std::fs::read(&lab.source).unwrap() == published,
        "no vault version is published"
    );
    assert!(lab.admission("m1").is_none() && lab.admission("m2").is_none());
}

#[test]
fn existing_vault_foundation_is_ready_and_idempotent() {
    let lab = lab("ready");
    let admin = admin_open(&lab.paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_machine("m2", &crate::transport::test_recipient(9))
        .unwrap();
    admin_publish(&lab.paths, &admin, &doc).unwrap();

    let first = admin_commons_init(&lab.paths).unwrap();
    assert!(first.published);
    assert_eq!(first.eligible, 2);
    assert_eq!(
        first.authority,
        admin_open(&lab.paths).unwrap().commons_authority().unwrap()
    );
    let published = std::fs::read(&lab.source).unwrap();
    let second = admin_commons_init(&lab.paths).unwrap();
    assert!(!second.published);
    assert_eq!(second.network, first.network);
    assert_eq!(second.authority, first.authority);
    assert_eq!(second.genesis, first.genesis);
    assert_eq!(second.eligible, first.eligible);
    assert_eq!(std::fs::read(&lab.source).unwrap(), published);
    assert!(
        crate::commons::bootstrap::Bootstrap::cached(&lab.paths)
            .unwrap()
            .unwrap()
            .access()
            .unwrap()
            .is_some()
    );
}

#[test]
fn retry_before_and_after_publication_keeps_one_network() {
    let lab = lab("retry");
    let admin = admin_open(&lab.paths).unwrap();
    let prepared = prepare(&lab.paths, &admin).unwrap();
    let first = admin_commons_init(&lab.paths).unwrap();
    assert_eq!(first.network, prepared.trust().network);
    assert_eq!(first.genesis, prepared.trust().genesis);

    std::fs::remove_file(lab.paths.dir.join(PREPARED)).unwrap();
    let recovered = admin_commons_init(&lab.paths).unwrap();
    assert_eq!(recovered.network, first.network);
    assert_eq!(recovered.authority, first.authority);
    assert_eq!(recovered.genesis, first.genesis);
    assert!(!recovered.published);
}

#[test]
fn prepared_descriptor_cannot_cross_authorities() {
    let first = lab("authority-a");
    let second = lab("authority-b");
    let admin = admin_open(&first.paths).unwrap();
    let descriptor = prepare(&first.paths, &admin).unwrap();
    aterm_journal::write_private_durable(&second.paths.dir, PREPARED, &descriptor.bytes()).unwrap();
    let error = admin_commons_init(&second.paths).unwrap_err();
    assert!(error.to_string().contains("another authority"), "{error}");
    assert!(
        admin_keys::control_descriptor(&admin_open(&second.paths).unwrap().doc)
            .unwrap()
            .is_none()
    );
}

#[test]
fn unrelated_local_history_is_never_replaced() {
    let lab = lab("history");
    let commons = lab.paths.dir.join("commons");
    fsx::ensure_private_dir(&commons).unwrap();
    aterm_journal::write_private_durable(&commons, "default.pin", b"existing history").unwrap();
    let error = admin_commons_init(&lab.paths).unwrap_err();
    assert!(
        error.to_string().contains("existing local history"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(commons.join("default.pin")).unwrap(),
        b"existing history"
    );
    assert!(
        admin_keys::control_descriptor(&admin_open(&lab.paths).unwrap().doc)
            .unwrap()
            .is_none()
    );
}

#[test]
fn opaque_control_and_unbound_authority_are_refused() {
    let opaque = lab("opaque");
    let admin = admin_open(&opaque.paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Hosts(vec!["m1".into()]),
        crate::commons::ENTRY,
        b"opaque",
    )
    .unwrap();
    admin_publish(&opaque.paths, &admin, &doc).unwrap();
    let error = admin_commons_init(&opaque.paths).unwrap_err();
    assert!(error.to_string().contains("opaque"), "{error}");

    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("unbound.vault");
    test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let error = admin_commons_init(&paths).unwrap_err();
    assert!(
        error.to_string().contains("no bound Commons authority"),
        "{error}"
    );
}

#[test]
fn retained_attempt_cannot_bypass_existing_history() {
    let lab = lab("retained-history");
    let admin = admin_open(&lab.paths).unwrap();
    prepare(&lab.paths, &admin).unwrap();
    let control = lab.paths.dir.join("commons").join("control");
    fsx::ensure_private_dir(&control).unwrap();
    let error = admin_commons_init(&lab.paths).unwrap_err();
    assert!(
        error.to_string().contains("existing local history"),
        "{error}"
    );
    assert!(
        admin_keys::control_descriptor(&admin_open(&lab.paths).unwrap().doc)
            .unwrap()
            .is_none()
    );
}

#[test]
fn published_root_wins_over_a_different_retained_attempt() {
    let other = lab("other-root");
    let lab = lab("root-mismatch");
    let first = admin_commons_init(&lab.paths).unwrap();
    let admin = admin_open(&other.paths).unwrap();
    let descriptor = prepare(&other.paths, &admin).unwrap();
    aterm_journal::write_private_durable(&lab.paths.dir, PREPARED, &descriptor.bytes()).unwrap();
    let published = std::fs::read(&lab.source).unwrap();
    let error = admin_commons_init(&lab.paths).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("differs from the retained attempt"),
        "{error}"
    );
    assert_eq!(std::fs::read(&lab.source).unwrap(), published);
    std::fs::remove_file(lab.paths.dir.join(PREPARED)).unwrap();
    let confirmed = admin_commons_init(&lab.paths).unwrap();
    assert_eq!(confirmed.network, first.network);
    assert_eq!(confirmed.genesis, first.genesis);
}

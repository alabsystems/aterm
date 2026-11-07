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
    assert!(
        published_descriptor(&lab.paths) == prepared.bytes(),
        "the retained descriptor is published byte for byte"
    );

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

/// Open `lab`'s vault, as `aterm secrets password open` does.
fn open(lab: &Lab) {
    let admin = admin_open(&lab.paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_open(true).unwrap();
    admin_publish(&lab.paths, &admin, &doc).unwrap();
}

/// `peer` enrolls in `lab`'s Commons through its admin's cached snapshot, as the LAN handshake
/// has it do.
fn enroll(lab: &Lab, peer: &Paths) -> crate::Result<crate::commons::node::Node> {
    use crate::commons::bootstrap::Bootstrap;
    sync(peer, None)?;
    let owner = Bootstrap::required(&lab.paths)?;
    let peer = Bootstrap::required(peer)?;
    let request = peer.request()?;
    let (mut pending, challenge) = crate::commons::binding::Pending::issue(
        &request,
        owner.trust().network,
        owner.trust().authority,
    )?;
    let answer = peer.device().answer(&request, &challenge)?;
    let mut journal =
        crate::commons::journal::Journal::open_existing(owner.journal_dir(), owner.trust())?;
    admin_cached(&lab.paths)?.enroll_commons_member(&owner, &mut journal, pending.verify(&answer)?)
}

/// The vault `lab` publishes, its bytes: what a call that publishes nothing leaves as it is.
fn published(lab: &Lab) -> Vec<u8> {
    std::fs::read(&lab.source).unwrap()
}

/// The control descriptor `paths`' vault publishes, its bytes.
fn published_descriptor(paths: &Paths) -> Vec<u8> {
    admin_keys::control_descriptor(&admin_open(paths).unwrap().doc)
        .unwrap()
        .expect("a published control descriptor")
        .bytes()
}

fn no_control(paths: &Paths) -> bool {
    admin_keys::control_descriptor(&admin_open(paths).unwrap().doc)
        .unwrap()
        .is_none()
}

/// An open vault's only admin founds its Commons by itself: the founder holds `Founder`, every
/// other member the open policy's `Open` (never a person's `Manual`), the audience names every
/// machine, and each member enrolls. A second founding publishes nothing.
/// NEGATIVE CONTROL: recording `Manual` for the members fails the method check.
#[test]
fn an_open_vaults_only_admin_founds_commons_and_every_member_enrolls() {
    let lab = lab("default-open");
    let plain = |admin: &Admin, doc: &mut Document, _: &str| {
        admin_publish(&lab.paths, admin, doc).unwrap();
    };
    let m2 = lab.machine("m2", plain);
    let m3 = lab.machine("m3", plain);
    open(&lab);
    let due = commons_foundable(&lab.paths).unwrap();
    assert!(
        matches!(&due, Foundable::Due(why) if why.contains("approving 2 of its 2 other machines")),
        "{due:?}"
    );
    assert_eq!(commons_foundable(&m2).unwrap(), Foundable::NotAdmin);

    let founded = admin_commons_found_by_default(&lab.paths).unwrap();
    assert!(founded.published);
    assert_eq!(founded.eligible, 3);
    assert_eq!(founded.approved, ["m1", "m2", "m3"]);
    let seq = admin_open(&lab.paths).unwrap().seq();
    let method = |host: &str| {
        let evidence = lab.admission(host).unwrap();
        assert_eq!(
            evidence.sequence, seq,
            "{host}: in the founding publication"
        );
        evidence.method
    };
    assert_eq!(method("m1"), AdmissionMethod::Founder);
    for host in ["m2", "m3"] {
        assert_eq!(method(host), AdmissionMethod::Open, "{host}");
    }
    assert_eq!(lab.audience(), ["m1", "m2", "m3"]);
    assert_eq!(commons_foundable(&lab.paths).unwrap(), Foundable::Founded);
    for peer in [&m2, &m3] {
        enroll(&lab, peer).unwrap();
    }

    let before = published(&lab);
    let again = admin_commons_found_by_default(&lab.paths).unwrap();
    assert!(!again.published && again.approved.is_empty(), "{again:?}");
    assert_eq!(
        (again.network, again.genesis),
        (founded.network, founded.genesis)
    );
    assert!(published(&lab) == before, "no vault version is published");
}

/// A closed vault is never founded by default: its members wait for a person's approval, which
/// `aterm secrets commons init` gives as it founds, so that command still approves them.
/// NEGATIVE CONTROL: dropping the open check reads the closed vault as due.
#[test]
fn a_closed_vault_is_left_to_commons_init() {
    let lab = lab("default-closed");
    lab.machine("m2", |admin, doc, _| {
        admin_publish(&lab.paths, admin, doc).unwrap();
    });
    let read = commons_foundable(&lab.paths).unwrap();
    assert!(
        matches!(&read, Foundable::Elsewhere(why) if why.contains("not open")),
        "{read:?}"
    );
    let before = published(&lab);
    let error = admin_commons_found_by_default(&lab.paths).unwrap_err();
    assert!(
        matches!(&error, NotFounded::Stands(why) if why.contains("not open")),
        "{error:?}"
    );
    assert!(published(&lab) == before && no_control(&lab.paths));
    let founded = admin_commons_init(&lab.paths).unwrap();
    assert_eq!(founded.approved, ["m1", "m2"]);
    assert_eq!(lab.admission("m2").unwrap().method, AdmissionMethod::Manual);
}

/// `commons init` on an open vault records the vault's standing approval (`Open`) for each
/// undecided member its open approver may have let in with nobody approving it, never a person's
/// `Manual`: the admin's own machine and a key on a `code-only` line (let in by a code or a
/// password, never by the open policy) stay `Manual`. Each still enrolls. On a root an earlier
/// aterm founded, the run that completes it records the same.
/// NEGATIVE CONTROL: recording `Manual` for every undecided machine fails the m2 check.
#[test]
fn commons_init_on_an_open_vault_records_the_open_policy_not_a_persons_approval() {
    let lab = lab("init-open");
    let plain = |admin: &Admin, doc: &mut Document, _: &str| {
        admin_publish(&lab.paths, admin, doc).unwrap();
    };
    let m2 = lab.machine("m2", plain);
    lab.machine("m3", |admin, doc, recipient| {
        doc.add_code_only(recipient).unwrap();
        admin_publish(&lab.paths, admin, doc).unwrap();
    });
    open(&lab);
    let founded = admin_commons_init(&lab.paths).unwrap();
    assert!(founded.published);
    assert_eq!(founded.approved, ["m1", "m2", "m3"]);
    let method = |host: &str| lab.admission(host).unwrap().method;
    assert_eq!(
        method("m1"),
        AdmissionMethod::Manual,
        "the admin who ran it"
    );
    assert_eq!(method("m2"), AdmissionMethod::Open, "the open policy's");
    assert_eq!(method("m3"), AdmissionMethod::Manual, "a code-only key");
    enroll(&lab, &m2).unwrap();

    // A root an earlier aterm founded, which recorded no decisions, completed on an open vault.
    let lab = lab_open("init-open-retry");
    lab.machine("m2", |admin, doc, _| {
        admin_publish(&lab.paths, admin, doc).unwrap();
    });
    found_without_decisions(&lab.paths);
    let retried = admin_commons_init(&lab.paths).unwrap();
    assert!(!retried.published);
    assert_eq!(retried.approved, ["m1", "m2"]);
    assert_eq!(lab.admission("m1").unwrap().method, AdmissionMethod::Manual);
    assert_eq!(lab.admission("m2").unwrap().method, AdmissionMethod::Open);
}

/// A vault that names a second admin is left to `aterm secrets commons init` on one of them,
/// since two founding at once could each publish a first roster; so is an unbound authority.
/// Neither is read as due, and a founding by default publishes nothing on either.
/// NEGATIVE CONTROL: dropping the sole-admin check founds the two-admin vault.
#[test]
fn a_vault_with_two_admins_or_an_unbound_authority_is_left_as_it_is() {
    let lab = lab("default-two-admins");
    lab.machine("m2", |admin, doc, _| {
        doc.add_admin("m2").unwrap();
        doc.set_open(true).unwrap();
        admin_publish(&lab.paths, admin, doc).unwrap();
    });
    let read = commons_foundable(&lab.paths).unwrap();
    assert!(
        matches!(&read, Foundable::Elsewhere(why) if why.contains("2 admins (m1, m2)")),
        "{read:?}"
    );
    let before = published(&lab);
    let error = admin_commons_found_by_default(&lab.paths).unwrap_err();
    assert!(
        matches!(&error, NotFounded::Stands(why) if why.contains("2 admins")),
        "{error:?}"
    );
    assert!(published(&lab) == before && no_control(&lab.paths));

    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("unbound.vault");
    test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let admin = admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_open(true).unwrap();
    admin_publish(&paths, &admin, &doc).unwrap();
    let read = commons_foundable(&paths).unwrap();
    assert!(
        matches!(&read, Foundable::Elsewhere(why) if why.contains("not bound")),
        "{read:?}"
    );
    let before = std::fs::read(&source).unwrap();
    let error = admin_commons_found_by_default(&paths).unwrap_err();
    assert!(
        matches!(&error, NotFounded::Stands(why) if why.contains("not bound")),
        "{error:?}"
    );
    assert!(std::fs::read(&source).unwrap() == before && no_control(&paths));
}

/// On a root an earlier aterm founded, a founding by default finishes it and records nothing:
/// the approvals that aterm left out stay `commons init`'s, a person's, and a recorded decision
/// stands. The read says founded, so no pass applies it.
#[test]
fn a_published_root_is_finished_and_no_decision_is_recorded_or_replaced() {
    let lab = lab("default-published");
    lab.machine("m2", |admin, doc, _| {
        admin_publish(&lab.paths, admin, doc).unwrap();
    });
    open(&lab);
    found_without_decisions(&lab.paths);
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
    let decided = lab.admission("m3").unwrap();
    assert_eq!(commons_foundable(&lab.paths).unwrap(), Foundable::Founded);
    let before = published(&lab);
    let finished = admin_commons_found_by_default(&lab.paths).unwrap();
    assert!(
        !finished.published && finished.approved.is_empty(),
        "{finished:?}"
    );
    assert!(published(&lab) == before, "no vault version is published");
    assert!(lab.admission("m1").is_none() && lab.admission("m2").is_none());
    assert_eq!(lab.admission("m3"), Some(decided));
}

/// A key on a `code-only` line, which an open vault's approver never lets in by itself, gets no
/// open approval at the founding; the rest do.
/// NEGATIVE CONTROL: dropping the code-only filter approves m3.
#[test]
fn a_code_only_key_is_not_approved_by_the_open_policy() {
    let lab = lab("default-code-only");
    lab.machine("m2", |admin, doc, _| {
        admin_publish(&lab.paths, admin, doc).unwrap();
    });
    lab.machine("m3", |admin, doc, recipient| {
        doc.add_code_only(recipient).unwrap();
        admin_publish(&lab.paths, admin, doc).unwrap();
    });
    open(&lab);
    let due = commons_foundable(&lab.paths).unwrap();
    assert!(
        matches!(&due, Foundable::Due(why) if why.contains("approving 1 of its 2 other machines")),
        "{due:?}"
    );
    let founded = admin_commons_found_by_default(&lab.paths).unwrap();
    assert_eq!(founded.approved, ["m1", "m2"]);
    assert_eq!(lab.admission("m2").unwrap().method, AdmissionMethod::Open);
    assert!(lab.admission("m3").is_none(), "left to approval by code");
}

/// Local Commons history the vault does not describe is never replaced, so it reads as
/// elsewhere's and a founding by default refuses it.
#[test]
fn unrelated_local_history_is_not_founded_over() {
    let lab = lab("default-history");
    open(&lab);
    let commons = lab.paths.dir.join("commons");
    fsx::ensure_private_dir(&commons).unwrap();
    aterm_journal::write_private_durable(&commons, "default.pin", b"existing history").unwrap();
    let read = commons_foundable(&lab.paths).unwrap();
    assert!(
        matches!(&read, Foundable::Elsewhere(why) if why.contains("existing local history")
            && why.contains("default.pin: move it out of the way")),
        "{read:?}"
    );
    let error = admin_commons_found_by_default(&lab.paths).unwrap_err();
    assert!(
        matches!(&error, NotFounded::Stands(why) if why.contains("move it out of the way")),
        "{error:?}"
    );
    assert!(no_control(&lab.paths));
}

/// A kill or power loss mid-write of the founding's own history leaves its temporary
/// (`.device.age.tmp.<pid>.<salt>`, as `create_temp_beside` names it) in `commons/`, which is no
/// unrelated history: the founding reads due and founds. A name of that shape beside any other
/// file, one off that shape, or any other name still refuses, naming the file.
/// NEGATIVE CONTROL: a check that knows only the exact names refuses the temporaries.
#[test]
fn a_leftover_temporary_of_the_foundings_own_history_is_no_unrelated_history() {
    let lab = lab_open("default-leftover");
    let commons = lab.paths.dir.join("commons");
    fsx::ensure_private_dir(&commons).unwrap();
    for leftover in [
        ".device.age.tmp.4242.0a1b2c3d",
        ".device.claim.tmp.17.deadbeef",
        ".runtime.lock.tmp.9.00000000",
    ] {
        std::fs::write(commons.join(leftover), b"half").unwrap();
    }
    let read = commons_foundable(&lab.paths).unwrap();
    assert!(matches!(read, Foundable::Due(_)), "{read:?}");
    for stranger in [
        ".default.pin.tmp.4242.0a1b2c3d",
        // A pid no leftover has, as a case-insensitive volume would take an uppercase twin
        // of a leftover's name for the leftover itself.
        ".device.age.tmp.5151.0A1B2C3D",
        ".device.age.tmp.4242.0a1b2c",
        ".device.age.tmp..0a1b2c3d",
        "device.age.tmp.4242.0a1b2c3d",
        ".DS_Store",
    ] {
        std::fs::write(commons.join(stranger), b"").unwrap();
        let read = commons_foundable(&lab.paths).unwrap();
        assert!(
            matches!(&read, Foundable::Elsewhere(why) if why.contains("existing local history")
                && why.contains(stranger)),
            "{stranger}: {read:?}"
        );
        std::fs::remove_file(commons.join(stranger)).unwrap();
    }
    let founded = admin_commons_found_by_default(&lab.paths).unwrap();
    assert!(founded.published, "{founded:?}");
    assert!(!no_control(&lab.paths));
}

/// An attempt this Mac kept is what a founding by default publishes, byte for byte; one kept
/// under another authority is never founded over.
/// NEGATIVE CONTROL: a read that skips the kept attempt's authority reads the second vault due.
#[test]
fn a_kept_attempt_is_finished_by_default_only_under_its_own_authority() {
    let lab = lab("default-kept");
    open(&lab);
    let kept = prepare(&lab.paths, &admin_open(&lab.paths).unwrap()).unwrap();
    let read = commons_foundable(&lab.paths).unwrap();
    assert!(
        matches!(&read, Foundable::Due(why) if why.contains("from the attempt it kept")),
        "{read:?}"
    );
    let founded = admin_commons_found_by_default(&lab.paths).unwrap();
    assert!(founded.published);
    // The network and genesis are the authority's canonical foundation, which any founding
    // under it shares; the first roster child is what only the kept attempt holds.
    assert!(
        published_descriptor(&lab.paths) == kept.bytes(),
        "the kept attempt is published byte for byte"
    );

    let other = lab_open("default-kept-other");
    aterm_journal::write_private_durable(&other.paths.dir, PREPARED, &kept.bytes()).unwrap();
    let kept_path = other.paths.dir.join(PREPARED).display().to_string();
    let read = commons_foundable(&other.paths).unwrap();
    assert!(
        matches!(&read, Foundable::Elsewhere(why) if why.contains("another authority")
            && why.contains(&format!("move {kept_path} out of the way"))),
        "{read:?}"
    );
    let error = admin_commons_found_by_default(&other.paths).unwrap_err();
    assert!(
        matches!(&error, NotFounded::Stands(why) if why.contains("another authority")
            && why.contains(&format!("move {kept_path} out of the way"))),
        "{error:?}"
    );
    assert!(no_control(&other.paths));
}

/// [`lab`], opened.
fn lab_open(name: &str) -> Lab {
    let lab = lab(name);
    open(&lab);
    lab
}

/// While a sync holds the vault's state lock the read is busy, as the Commons observation reads
/// it, never an error: a member's row reads not applicable, not unknown. Once the lock is let go
/// the same read finds the founding due.
/// NEGATIVE CONTROL: a read that turns the held lock into an error fails the busy check.
#[test]
fn a_held_state_lock_reads_busy_not_an_error() {
    let lab = lab_open("default-busy");
    let held = StateLock::take(&lab.paths).unwrap();
    let read = commons_foundable(&lab.paths);
    assert!(matches!(read, Ok(Foundable::Busy)), "{read:?}");
    drop(held);
    let read = commons_foundable(&lab.paths).unwrap();
    assert!(matches!(read, Foundable::Due(_)), "{read:?}");
}

/// Where the person turned this Mac's vault sync off (`aterm secrets agent off`), no pass founds
/// its Commons here, since a founding publishes to the vault and syncs this Mac's copy; the
/// read names the switch that lets it, and turning sync back on finds the founding due.
/// NEGATIVE CONTROL: a read that skips the off record reads the founding due.
#[test]
fn sync_turned_off_by_the_person_founds_nothing() {
    let lab = lab_open("default-agent-off");
    crate::fsx::ensure_private_dir(&lab.paths.dir).unwrap();
    let off = crate::service::subscriber_off_path(&lab.paths.home);
    std::fs::write(&off, b"off by choice\n").unwrap();
    let read = commons_foundable(&lab.paths).unwrap();
    assert!(
        matches!(&read, Foundable::Elsewhere(why) if why.contains("aterm secrets agent on")),
        "{read:?}"
    );
    std::fs::remove_file(&off).unwrap();
    let read = commons_foundable(&lab.paths).unwrap();
    assert!(matches!(read, Foundable::Due(_)), "{read:?}");
}

/// While a program this Mac's subscriber approves with is a release that shares no Commons root
/// on an open approval (0.142.0, as m22's held snapshot is, through 0.144.0), or one whose
/// release cannot be read, no pass founds Commons here: each Mac that approver lets in afterwards
/// would hold an `Open` decision outside the control entry's audience, which later aterms read as
/// an exclusion. The read waits on a person, naming the program, its release and the remedies,
/// and the founding refuses before it prepares anything. The program the job starts next counts
/// as the one running does. An approver at 0.145.0 lets the same vault be founded.
/// NEGATIVE CONTROL: dropping either gate founds behind the 0.142.0 approver; a threshold of
/// 0.144.0 founds behind the 0.144.0 one.
#[test]
fn an_approver_that_shares_no_root_on_an_open_approval_holds_the_founding() {
    use crate::client::locker_skew::{PLAYED, Running};
    const SNAPSHOT: &str = "/Users//a/.local/share/aterm-secrets/aterm";
    const NEXT: &str = "/Users//a/.local/share/aterm-secrets/next/aterm";
    let at =
        |pid: Option<u32>, program: &str, version: Result<locker_skew::Version, String>| Running {
            pid,
            program: PathBuf::from(program),
            version,
        };
    let play = |programs: Vec<Running>| PLAYED.with(|played| *played.borrow_mut() = Some(programs));
    let lab = lab("default-old-approver");
    let m2 = lab.machine("m2", |admin: &Admin, doc: &mut Document, _: &str| {
        admin_publish(&lab.paths, admin, doc).unwrap();
    });
    open(&lab);
    let before = published(&lab);
    let holds = |release: &str| {
        let read = commons_foundable(&lab.paths).unwrap();
        let Foundable::Waiting(why) = &read else {
            panic!("an approver at {release} holds the founding: {read:?}");
        };
        let error = admin_commons_found_by_default(&lab.paths)
            .expect_err("an old approver refuses the founding");
        assert!(
            matches!(&error, NotFounded::Stands(why) if why.contains(release)),
            "{error:?}"
        );
        assert_eq!(published(&lab), before, "nothing was published");
        assert!(
            !lab.paths.dir.join(PREPARED).exists(),
            "nothing was prepared"
        );
        why.clone()
    };

    play(vec![at(Some(8000), SNAPSHOT, Ok((0, 142, 0)))]);
    let why = holds("aterm 0.142.0");
    for part in [
        "vault default-old-approver: ",
        SNAPSHOT,
        "aterm 0.142.0) shares no Commons root",
        "as aterm 0.145.0 and later do",
        "`aterm secrets agent on`",
        "`agent on --program` onto an aterm 0.145.0 or later",
        "`aterm secrets commons init` founds it now",
        "`aterm secrets approve <its age1… key> --name <host>`",
    ] {
        assert!(why.contains(part), "{part}: {why}");
    }
    // The last release before the threshold holds it too.
    play(vec![at(Some(8000), SNAPSHOT, Ok((0, 144, 0)))]);
    holds("aterm 0.144.0");
    // A subscriber that runs a sharing release but restarts into an old one holds it.
    play(vec![
        at(Some(8000), SNAPSHOT, Ok((0, 145, 0))),
        at(None, NEXT, Ok((0, 142, 0))),
    ]);
    assert!(holds("aterm 0.142.0").contains(NEXT));
    // An approver whose release cannot be read counts as old, and is not said to share none.
    play(vec![at(Some(8000), SNAPSHOT, Err("timed out".to_owned()))]);
    let why = holds("release is unknown: timed out");
    assert!(why.contains("may share no Commons root"), "{why}");

    // CONTROL: the same vault, its approver at 0.145.0, is founded and its member enrolls.
    play(vec![at(Some(8000), SNAPSHOT, Ok((0, 145, 0)))]);
    let read = commons_foundable(&lab.paths).unwrap();
    assert!(matches!(read, Foundable::Due(_)), "{read:?}");
    let founded = admin_commons_found_by_default(&lab.paths).unwrap();
    assert!(founded.published);
    assert!(founded.approved.contains(&"m2".to_owned()), "{founded:?}");
    enroll(&lab, &m2).unwrap();
    PLAYED.with(|played| *played.borrow_mut() = None);
}

thread_local! {
    /// A test's hook, run once by the next [`finish`] on this thread between its sync and its
    /// read-back: this Mac's subscriber, which the founding's publication woke, taking the vault's
    /// state lock to apply what it heard.
    static SYNCED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run by [`finish`] between its sync and its read-back: the hook [`SYNCED`] holds, once.
pub(super) fn synced() {
    if let Some(synced) = SYNCED.with(|hook| hook.borrow_mut().take()) {
        synced();
    }
}

/// A founding's read-back meets this Mac's subscriber, which the publication woke, holding the
/// vault's state lock for a moment: it waits that out and reads the root the founding published,
/// where a read at once finds the state busy and the founding fails with its root published.
/// The subscriber takes the lock inside the founding's own `finish`, between its sync and its
/// read-back ([`SYNCED`]), and lets it go on the read's own first blocked try, so no scheduling
/// races the wait; a lock still held at the deadline says the vault is busy.
/// NEGATIVE CONTROL: a read-back that reads once (`Bootstrap::cached`, `Bootstrap::observe`) or by
/// a deadline already passed fails the founding while the lock is held.
#[test]
fn the_read_back_waits_out_the_subscriber_its_publication_woke() {
    let lab = lab_open("read-back");
    let subscriber = std::rc::Rc::new(std::cell::RefCell::new(None::<StateLock>));
    let (paths, applying) = (lab.paths.clone(), std::rc::Rc::clone(&subscriber));
    SYNCED.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            *applying.borrow_mut() = Some(StateLock::take(&paths).unwrap());
        }));
    });
    let waited = std::rc::Rc::new(std::cell::Cell::new(0));
    let (count, applied) = (std::rc::Rc::clone(&waited), std::rc::Rc::clone(&subscriber));
    let founded = crate::commons::bootstrap::tests::while_held(
        move || {
            count.set(count.get() + 1);
            drop(applied.borrow_mut().take());
        },
        || admin_commons_found_by_default(&lab.paths),
    );
    assert!(
        SYNCED.with(|hook| hook.borrow().is_none()),
        "CONTROL: the subscriber took the lock in the founding's finish"
    );
    assert_eq!(
        waited.get(),
        1,
        "the read-back found the state lock held once"
    );
    let founded = founded.unwrap();
    assert!(founded.published);

    let held = StateLock::take(&lab.paths).unwrap();
    let Err(busy) = installed(&lab.paths, Instant::now()) else {
        panic!("a state lock held at the deadline reads busy");
    };
    assert_eq!(busy.kind(), crate::ErrorKind::VaultBusy, "{busy}");
    assert!(
        busy.to_string().contains("not visible after sync"),
        "{busy}"
    );
    drop(held);
    let read = installed(&lab.paths, Instant::now()).unwrap();
    assert_eq!(read.trust().genesis, founded.genesis);
}

/// A founding by default meets another founding on this Mac (a person's `commons init`) only
/// through the founding lock: while that holds it, the pass prepares and publishes nothing and
/// says what runs; once it is let go the pass founds.
/// NEGATIVE CONTROL: a founding by default that takes no lock publishes while it is held.
#[test]
fn a_held_founding_lock_puts_the_founding_by_default_off() {
    let lab = lab_open("default-locked");
    let before = published(&lab);
    let held = FoundationLock::held_waiting(&lab.paths).unwrap();
    let error = admin_commons_found_by_default(&lab.paths).unwrap_err();
    assert!(
        matches!(&error, NotFounded::Again(why)
            if why.contains("another founding of this vault's Commons runs on this Mac now")),
        "{error:?}"
    );
    assert_eq!(published(&lab), before, "nothing was published");
    assert!(
        !lab.paths.dir.join(PREPARED).exists(),
        "nothing was prepared"
    );
    drop(held);
    assert!(
        admin_commons_found_by_default(&lab.paths)
            .unwrap()
            .published
    );
}

/// A person's `commons init` waits out another founding on this Mac (a window's pass) that holds
/// the founding lock, then founds. The holder lets it go on the waiter's own first blocked try
/// (`aterm_uds::file_lock::with_lock_wait_observer`), so no scheduling races the wait.
/// NEGATIVE CONTROL: a `commons init` that takes no lock founds without waiting on it.
#[test]
fn commons_init_waits_out_another_founding_on_this_mac() {
    let lab = lab_open("init-waits");
    let mut window = Some(FoundationLock::held_if_free(&lab.paths).unwrap().unwrap());
    assert!(
        FoundationLock::held_if_free(&lab.paths).unwrap().is_none(),
        "CONTROL: a second take finds it held"
    );
    let waited = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = std::rc::Rc::clone(&waited);
    let founded = aterm_uds::file_lock::with_lock_wait_observer(
        move |_| {
            count.set(count.get() + 1);
            drop(window.take());
        },
        || admin_commons_init(&lab.paths),
    )
    .unwrap();
    assert_eq!(
        waited.get(),
        1,
        "commons init found the founding lock held once"
    );
    assert!(founded.published);
}

/// Run `call` on `lab` with `race` published in the moment between `call`'s read of the vault
/// and its publication's draft: that publication finds this thread's draft turn taken
/// (`log_entries::TURN_WAITS`), the turn is let go, and `race` publishes first, so `call`'s
/// publication is not newer than the one the vault holds (a server's 409).
fn raced<T>(lab: &Lab, race: impl FnOnce(&Paths) + 'static, call: impl FnOnce(&Paths) -> T) -> T {
    let turn = log_entries::draft_turn(&lab.paths, None).unwrap();
    let paths = lab.paths.clone();
    let ran = std::rc::Rc::new(std::cell::Cell::new(false));
    let raced = std::rc::Rc::clone(&ran);
    log_entries::TURN_WAITS.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            drop(turn);
            race(&paths);
            raced.set(true);
        }));
    });
    let out = call(&lab.paths);
    assert!(
        ran.get(),
        "CONTROL: the race ran in the founding's publication"
    );
    out
}

/// A founding whose publication met another that landed first (a server's 409, which stored
/// nothing of it) reopens the vault once. Where that other publication carried the same root (a
/// founding from the attempt this Mac kept), it is finished there with nothing more published,
/// by default and at `commons init`; where it carried none (an approver's publish), the
/// founding says another publish landed first, and the next founds from the kept attempt.
/// NEGATIVE CONTROL: dropping the reopen leaves the vault's bare refusal, which says no publish
/// landed first.
#[test]
fn a_publication_another_landed_first_finishes_the_root_that_landed() {
    let lab = lab_open("landed-first-none");
    let error = raced(
        &lab,
        |paths| {
            let admin = admin_open(paths).unwrap();
            let mut doc = admin.doc.clone();
            doc.set_entry(&Audience::Everyone, "raced", b"an approver's publish")
                .unwrap();
            admin_publish(paths, &admin, &doc).unwrap();
        },
        admin_commons_found_by_default,
    )
    .unwrap_err();
    assert!(
        matches!(&error, NotFounded::Again(why) if why.contains("another publish landed first")),
        "{error:?}"
    );
    assert!(no_control(&lab.paths));
    let kept = prepared(&lab.paths)
        .unwrap()
        .expect("the attempt stays kept");
    let founded = admin_commons_found_by_default(&lab.paths).unwrap();
    assert!(founded.published);
    assert_eq!(founded.genesis, kept.trust().genesis);

    for init in [false, true] {
        let lab = lab_open(if init {
            "landed-first-init"
        } else {
            "landed-first-default"
        });
        let done = if init {
            raced(&lab, found_without_decisions, admin_commons_init).unwrap()
        } else {
            raced(
                &lab,
                found_without_decisions,
                admin_commons_found_by_default,
            )
            .unwrap()
        };
        assert!(
            !done.published,
            "init={init}: the root that landed is finished"
        );
        assert_eq!(
            published_descriptor(&lab.paths),
            prepared(&lab.paths).unwrap().unwrap().bytes(),
            "init={init}: the kept attempt is the root that landed"
        );
        assert_eq!(
            done.genesis,
            prepared(&lab.paths).unwrap().unwrap().trust().genesis
        );
    }
}

/// A founding by default whose publication met another that landed first reads the root that
/// landed, reopened, as its first read reads one: a root that is not the one this Mac kept stands,
/// as the next pass's first read of it does, and promises no retry.
/// NEGATIVE CONTROL: the reopened root read through `From<Error>` reads the mismatch, an error of
/// no kind, as one that passes, which the next pass then reads as standing.
#[test]
fn a_root_that_landed_first_and_is_not_the_kept_one_stands() {
    let other = lab("landed-first-other-root");
    let elsewhere = prepare(&other.paths, &admin_open(&other.paths).unwrap()).unwrap();
    let lab = lab_open("landed-first-not-kept");
    let error = raced(
        &lab,
        move |paths| {
            found_without_decisions(paths);
            aterm_journal::write_private_durable(&paths.dir, PREPARED, &elsewhere.bytes()).unwrap();
        },
        admin_commons_found_by_default,
    )
    .unwrap_err();
    assert!(
        matches!(&error, NotFounded::Stands(why)
            if why.contains("differs from the retained attempt")
                && why.contains("`aterm secrets commons init`")),
        "{error:?}"
    );
    assert_eq!(
        admin_commons_found_by_default(&lab.paths),
        Err(error),
        "the next pass's first read meets the same answer"
    );
    drop(other);
}

/// What a founding by default that meets the same answer until a person changes something reads
/// as, against one that may pass by the next pass: an error whose kind says the next try meets
/// the same answer (the vault server's 403 or 507) stands, naming `aterm secrets commons init`;
/// a lost race, a deadline, a busy lock or vault and an error whose kind says nothing pass, word
/// for word. A precondition the founding refuses stands.
/// NEGATIVE CONTROL: reading every error as passing turns the refusal into a retry.
#[test]
fn a_refusal_stands_and_a_lost_race_passes() {
    let stands = NotFounded::from(Error::of_kind(crate::ErrorKind::Refused, "answered 403"));
    assert!(
        matches!(&stands, NotFounded::Stands(why) if why.starts_with("answered 403: ")
            && why.contains("`aterm secrets commons init`")),
        "{stands:?}"
    );
    for kind in [
        crate::ErrorKind::NotStored,
        crate::ErrorKind::TimedOut,
        crate::ErrorKind::JournalBusy,
        crate::ErrorKind::VaultBusy,
        crate::ErrorKind::Other,
    ] {
        assert_eq!(
            NotFounded::from(Error::of_kind(kind, "x")),
            NotFounded::Again("x".to_owned()),
            "{kind:?}"
        );
    }
    let two = lab("stands-two-admins");
    two.machine("m2", |admin, doc, _| {
        doc.add_admin("m2").unwrap();
        doc.set_open(true).unwrap();
        admin_publish(&two.paths, admin, doc).unwrap();
    });
    let refused = admin_commons_found_by_default(&two.paths);
    assert!(
        matches!(&refused, Err(NotFounded::Stands(why)) if why.contains("2 admins")),
        "{refused:?}"
    );
}

/// Each reason an admin's aterm does not found its vault's Commons by itself names what changes
/// it where anything does: an opaque control entry names the edit that removes it, and that edit
/// leaves the vault to be founded; another Mac as the only admin names `commons init` run there;
/// an unbound authority says no command binds one; a `commons` path that is a file names the move
/// that lets the founding go on.
/// NEGATIVE CONTROL: the bare reasons, with no remedy, fail each check.
#[test]
fn each_reason_not_to_found_by_default_names_what_changes_it() {
    let lab = lab_open("default-remedies");
    let opaque = || Audience::Hosts(vec!["m1".into()]);
    let admin = admin_open(&lab.paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&opaque(), crate::commons::ENTRY, b"opaque")
        .unwrap();
    admin_publish(&lab.paths, &admin, &doc).unwrap();
    let read = commons_foundable(&lab.paths).unwrap();
    let edit = format!(
        "an admin's `aterm secrets edit` that removes its `{}` entry leaves the vault to be founded",
        crate::commons::ENTRY
    );
    assert!(
        matches!(&read, Foundable::Elsewhere(why) if why.contains("opaque") && why.contains(&edit)),
        "{read:?}"
    );
    // The edit it names, made as an admin's edit publishes it: the vault reads due.
    let admin = admin_open(&lab.paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.unset_entry(&opaque(), crate::commons::ENTRY).unwrap();
    admin_publish(&lab.paths, &admin, &doc).unwrap();
    let read = commons_foundable(&lab.paths).unwrap();
    assert!(matches!(read, Foundable::Due(_)), "{read:?}");

    let doc = admin_open(&lab.paths).unwrap().doc.clone();
    let why = founds_by_default(&doc, true, "age1notthismac").unwrap_err();
    assert_eq!(
        why,
        "this Mac is not its admin: `aterm secrets commons init`, run on m1, founds its Commons"
    );
    let why = founds_by_default(&doc, false, "age1notthismac").unwrap_err();
    assert!(
        why.contains("not bound") && why.contains("no command binds one to an existing vault"),
        "{why}"
    );

    let commons = lab.paths.dir.join("commons");
    assert!(!commons.exists(), "CONTROL: nothing founded or kept yet");
    std::fs::write(&commons, b"").unwrap();
    let read = commons_foundable(&lab.paths).unwrap();
    assert!(
        matches!(&read, Foundable::Elsewhere(why)
            if why.contains(&format!("cannot replace the existing commons path {}", commons.display()))
                && why.contains("move it out of the way")),
        "{read:?}"
    );
}

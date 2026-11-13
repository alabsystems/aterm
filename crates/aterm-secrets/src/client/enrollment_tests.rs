// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::binding::Pending;

/// A vault `create` made at a path, with Commons control, and the pin a machine joins by.
fn founded(root: &Path) -> (Paths, String) {
    let paths = Paths::for_home(&root.join("m1"));
    let source = root.join("vault");
    let created = create(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    (paths, format!("{}#{}", source.display(), created.signer))
}

/// `owner` lets `host` in as an open admission, as `aterm secrets password open` records one
/// ([`admin_publish_opened`]), and `host` joins. Returns the new machine's paths.
fn admitted_open(root: &Path, owner: &Paths, pinned: &str, host: &str) -> Paths {
    let machine = Paths::for_home(&root.join(host));
    let recipient = machine_identity(&machine).unwrap().recipient().to_bech32();
    let admin = admin_cached(owner).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_machine(host, &recipient).unwrap();
    publish_as(
        owner,
        &admin,
        &doc,
        false,
        &[(recipient, AdmissionMethod::Open)],
        false,
    )
    .unwrap();
    join(
        &machine,
        &JoinOptions {
            source: pinned.into(),
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

/// `owner` publishes its vault with `hosts` as the control entry's audience.
fn share(owner: &Paths, hosts: &[&str]) {
    let admin = admin_cached(owner).unwrap();
    let mut doc = admin.doc.clone();
    let audience = doc.commons_audience().unwrap().clone();
    let value = doc
        .entries()
        .into_iter()
        .find(|e| e.name == ENTRY)
        .unwrap()
        .value;
    doc.unset_entry(&audience, ENTRY).unwrap();
    let hosts = hosts.iter().map(|h| (*h).into()).collect();
    doc.set_entry(&Audience::Hosts(hosts), ENTRY, value.as_slice())
        .unwrap();
    admin_publish(owner, &admin, &doc).unwrap();
}

fn in_audience(owner: &Paths, host: &str) -> bool {
    matches!(
        admin_cached(owner).unwrap().published_doc.commons_audience(),
        Some(Audience::Hosts(hosts)) if hosts.iter().any(|h| h == host)
    )
}

/// A possession proof from `peer` for `owner`'s root, as the LAN handshake makes one.
fn possession(owner: &Bootstrap, peer: &Bootstrap) -> Possession {
    let request = peer.request().unwrap();
    let trust = owner.trust();
    let (mut pending, challenge) =
        Pending::issue(&request, trust.network, trust.authority).unwrap();
    let answer = peer.device().answer(&request, &challenge).unwrap();
    pending.verify(&answer).unwrap()
}

/// `owner`'s cached admin enrolls `proof` into its own control journal.
fn enroll(owner: &Paths, proof: Possession) -> crate::Result<Node> {
    let bootstrap = Bootstrap::required(owner)?;
    let mut journal = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust())?;
    admin_cached(owner)?.enroll_commons_member(&bootstrap, &mut journal, proof)
}

#[test]
fn an_open_admission_shares_the_root_and_enrolls_while_in_the_audience() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (owner, pinned) = founded(root.path());
    let m2 = admitted_open(root.path(), &owner, &pinned, "m2");
    let admin = admin_cached(&owner).unwrap();
    assert_eq!(
        admin.commons_admission("m2").unwrap().method,
        AdmissionMethod::Open
    );
    assert!(
        in_audience(&owner, "m2"),
        "an open approval shares the control entry in its own publication"
    );
    let peer = Bootstrap::required(&m2).unwrap();
    let proof = possession(&Bootstrap::required(&owner).unwrap(), &peer);
    // An explicit exclusion between the challenge and the enrollment still refuses.
    share(&owner, &["m1"]);
    let refused = enroll(&owner, proof).unwrap_err().to_string();
    assert!(refused.contains("eligible recipient"), "{refused}");
    // Shared again, the same open decision enrolls it.
    share(&owner, &["m1", "m2"]);
    let proof = possession(&Bootstrap::required(&owner).unwrap(), &peer);
    enroll(&owner, proof).unwrap();
    let owner_root = Bootstrap::required(&owner).unwrap();
    let journal = Journal::open_existing(owner_root.journal_dir(), owner_root.trust()).unwrap();
    let access = owner_root.access_in(&journal).unwrap().unwrap();
    assert!(
        access
            .members()
            .iter()
            .any(|m| m.binding().recipient() == machine_identity(&m2).unwrap().recipient())
    );
}

#[test]
fn a_removed_open_member_put_back_by_a_plain_edit_cannot_enroll() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (owner, pinned) = founded(root.path());
    let m2 = admitted_open(root.path(), &owner, &pinned, "m2");
    let peer = Bootstrap::required(&m2).unwrap();
    let recipient = machine_identity(&m2).unwrap().recipient().to_bech32();
    admin_revoke(&owner, &admin_open(&owner).unwrap(), "m2", false).unwrap();
    // Put back by an edit that records no decision, inside the audience.
    let admin = admin_cached(&owner).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_machine("m2", &recipient).unwrap();
    admin_publish(&owner, &admin, &doc).unwrap();
    share(&owner, &["m1", "m2"]);
    assert!(
        admin_cached(&owner)
            .unwrap()
            .commons_admission("m2")
            .is_none(),
        "removal forgets the open decision"
    );
    let proof = possession(&Bootstrap::required(&owner).unwrap(), &peer);
    let refused = enroll(&owner, proof).unwrap_err().to_string();
    assert!(refused.contains("admission evidence"), "{refused}");
}

#[test]
fn sequential_approvals_rehome_one_control_audience() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    create(
        &paths,
        root.path().join("vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let mut doc = admin_cached(&paths).unwrap().doc.clone();
    let initial = doc.lockers().len();
    for n in 2..=30 {
        let recipient = Identity::from_secret_bytes([n; 32]).recipient().to_bech32();
        doc.add_machine(&format!("m{n}"), &recipient).unwrap();
        doc = include_approved(&doc, &[(recipient, AdmissionMethod::Manual)])
            .unwrap()
            .unwrap();
        assert_eq!(
            doc.lockers().len(),
            initial,
            "approval {n} left an empty old audience"
        );
    }
}

#[test]
fn cached_admin_requires_applied_membership_and_exact_snapshot() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("vault");
    create(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let original = load_state(&paths).unwrap();
    std::fs::remove_file(source).unwrap();
    assert!(admin_cached(&paths).is_ok(), "no source fetch is necessary");
    for mutation in 0..5 {
        let mut state = original.clone();
        match mutation {
            0 => state.failed = true,
            1 => state.membership = Stored::Revoked,
            2 => state.source_identity = None,
            3 => state.seq += 1,
            4 => state.digest = Some([17; 32]),
            _ => unreachable!(),
        }
        save_state(&paths, &state).unwrap();
        assert!(
            admin_cached(&paths).is_err(),
            "accepted state mutation {mutation}"
        );
    }
}

/// The envelopes this thread parsed and the signatures it asked for while `ask` ran.
fn envelope_work(ask: impl FnOnce()) -> (usize, usize) {
    let now = || {
        (
            envelope::TEST_PARSES.with(std::cell::Cell::get),
            envelope::TEST_SIGNATURES.with(std::cell::Cell::get),
        )
    };
    let before = now();
    ask();
    let after = now();
    (after.0 - before.0, after.1 - before.1)
}

/// A node's watch asks at each tick whether this device opens the vault's admin custody
/// (`alab_tcp_role` in aterm-agent's node service). Over an unchanged held vault no ask after
/// the first parses an envelope or asks for a signature: the held entry verified its own
/// once. A new version, and a file of the same length renamed over it, are each verified again,
/// once; one whose signature fails is refused at every ask with the reader's own error, and
/// the applied bytes put back open again.
/// NEGATIVE CONTROL: with `admin_cached_observed` giving the held bytes to
/// `envelope::verify_for_reader`, the eight asks over the unchanged vault parse eight
/// envelopes and ask for eight signatures (`left: (8, 8)`, `right: (0, 0)`).
#[test]
fn cached_admin_verifies_an_unchanged_held_vault_once() {
    const ASKS: usize = 8;
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, _) = founded(root.path());
    let held = || {
        super::super::chain_cache::held(&paths.cache_file())
            .unwrap()
            .expect("a held vault")
    };
    let asks = |seq: u64| {
        envelope_work(|| {
            for _ in 0..ASKS {
                assert_eq!(admin_cached(&paths).unwrap().seq, seq);
            }
        })
    };

    let first = admin_cached(&paths).unwrap().seq;
    let entry = held();
    assert_eq!(entry.signature_checks(), 1);
    assert_eq!(asks(first), (0, 0), "an unchanged held vault");
    assert_eq!(entry.signature_checks(), 1);

    // A new version: other bytes, verified once.
    share(&paths, &["m1"]);
    let second = admin_cached(&paths).unwrap().seq;
    assert!(second > first);
    let applied = held();
    assert!(!std::sync::Arc::ptr_eq(&entry, &applied));
    assert_eq!(applied.signature_checks(), 1);
    assert_eq!(asks(second), (0, 0), "the new version, unchanged");
    assert_eq!(applied.signature_checks(), 1);

    // A file of the same length renamed over it, one signed byte changed: read and verified
    // again, once, and refused at every ask as the reader refuses those bytes.
    let mut forged = applied.bytes().to_vec();
    let time = forged
        .windows(7)
        .position(|at| at == b"\ntime: ")
        .expect("a time header");
    let digit = &mut forged[time + 7];
    *digit = if *digit == b'1' { b'2' } else { b'1' };
    assert_eq!(forged.len(), applied.bytes().len());
    let config = config(&paths).unwrap().unwrap();
    let refusal = envelope::verify_for_reader(
        &forged,
        &config.name,
        &config.signer.unwrap(),
        &config.followed,
        None,
    )
    .unwrap_err()
    .to_string();
    crate::fsx::write_atomic(&paths.cache_file(), &forged, 0o600).unwrap();
    let refused = || admin_cached(&paths).err().map(|error| error.to_string());
    assert_eq!(
        envelope_work(|| assert_eq!(refused().as_deref(), Some(refusal.as_str()))),
        (1, 1),
        "other bytes are parsed and verified"
    );
    let rewritten = held();
    assert!(!std::sync::Arc::ptr_eq(&applied, &rewritten));
    assert_eq!(rewritten.signature_checks(), 1);
    assert_eq!(
        envelope_work(|| {
            for _ in 0..ASKS {
                assert_eq!(refused().as_deref(), Some(refusal.as_str()));
            }
        }),
        (0, 0),
        "refused at every ask from the one check"
    );
    assert_eq!(rewritten.signature_checks(), 1);

    // The applied bytes renamed back: another file, the same bytes, so the same entry.
    crate::fsx::write_atomic(&paths.cache_file(), applied.bytes(), 0o600).unwrap();
    assert_eq!(asks(second), (0, 0), "the applied bytes again");
    assert!(std::sync::Arc::ptr_eq(&applied, &held()));
    assert_eq!(applied.signature_checks(), 1);
}

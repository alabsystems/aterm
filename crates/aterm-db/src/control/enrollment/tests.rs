// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_net::lan::{Connection, Scope};
use aterm_secrets::{
    client::{self, JoinOptions, Paths},
    commons::bootstrap::Bootstrap,
};
use std::{net::TcpListener, thread};

pub(crate) struct Lab {
    pub(crate) root: aterm_tempfile::TempDir,
    pub(crate) paths: [Paths; 2],
    source: std::path::PathBuf,
}
impl Lab {
    pub(crate) fn new(qualified: bool) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let source = root.path().join("lab.vault");
        let paths = [
            Paths::for_home(&root.path().join("m1")),
            Paths::for_home(&root.path().join("m2")),
        ];
        let created =
            client::create(&paths[0], source.to_str().unwrap(), None, Some("m1")).unwrap();
        let peer = client::machine_identity(&paths[1])
            .unwrap()
            .recipient()
            .to_bech32();
        let admin = client::admin_open(&paths[0]).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m2", &peer).unwrap();
        if qualified {
            client::admin_publish_approved(&paths[0], &admin, &doc, &[peer]).unwrap();
        } else {
            let control = doc
                .entries()
                .into_iter()
                .find(|e| e.name == aterm_secrets::commons::ENTRY)
                .unwrap()
                .value;
            doc.set_entry(
                &aterm_secrets::text::Audience::Hosts(vec!["m1".into(), "m2".into()]),
                aterm_secrets::commons::ENTRY,
                control.as_slice(),
            )
            .unwrap();
            client::admin_publish(&paths[0], &admin, &doc).unwrap();
        }
        client::join(
            &paths[1],
            &JoinOptions {
                source: format!("{}#{}", source.display(), created.signer),
                host: Some("m2".into()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: &|| None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        Self {
            root,
            paths,
            source,
        }
    }
    pub(crate) fn bootstrap(&self, n: usize) -> Bootstrap {
        Bootstrap::required(&self.paths[n]).unwrap()
    }
    pub(crate) fn endpoint(&self, n: usize) -> Endpoint {
        Endpoint::from_bootstrap(&self.bootstrap(n))
            .unwrap()
            .unwrap()
            .with_commons(
                self.paths[n].clone(),
                self.root.path().join(format!("config-{n}.toml")),
            )
            .unwrap()
    }
    pub(crate) fn pending(&self) -> Endpoint {
        Endpoint::pending_from_bootstrap(&self.bootstrap(1), self.paths[1].clone()).unwrap()
    }
    pub(crate) fn exchange(&self, peer: &Endpoint) -> (Result<Report>, Result<Report>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let owner = self.endpoint(0);
        thread::scope(|s| {
            let server = s.spawn(|| {
                owner.exchange(
                    Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap(),
                    Side::Responder,
                    &mut Repair::default(),
                )
            });
            let client = peer.exchange(
                Connection::connect(addr, Scope::Loopback, Duration::from_secs(60)).unwrap(),
                Side::Initiator,
                &mut Repair::default(),
            );
            (server.join().unwrap(), client)
        })
    }
}

#[test]
fn second_device_enrolls_over_tls_and_retries_without_a_vault_server_or_new_epoch() {
    let lab = Lab::new(true);
    let peer = lab.pending();
    assert!(lab.bootstrap(1).witness().unwrap().is_none());
    assert!(
        !lab.bootstrap(1)
            .access()
            .is_ok_and(|access| access.is_some())
    );
    std::fs::rename(&lab.source, lab.root.path().join("offline.vault")).unwrap();
    let (server, client) = lab.exchange(&peer);
    assert_eq!(server.unwrap().sent, 1);
    assert_eq!(client.unwrap().received, 1);
    let b = lab.bootstrap(1);
    let access = b.access().unwrap().unwrap();
    assert_eq!(access.members().len(), 2);
    assert_eq!(
        access.local().role(),
        aterm_secrets::commons::roster::Role::Writer
    );
    let first = access.generation();
    let (server, client) = lab.exchange(&peer);
    server.unwrap();
    client.unwrap();
    assert_eq!(
        lab.bootstrap(1).access().unwrap().unwrap().generation(),
        first
    );
    let regular = lab.endpoint(1);
    assert!(!regular.pending());
    let (server, client) = lab.exchange(&regular);
    server.unwrap();
    client.unwrap();
}

#[test]
fn unrecorded_recipient_with_a_copied_audience_cannot_enroll() {
    let lab = Lab::new(false);
    let before = lab.bootstrap(0).access().unwrap().unwrap().generation();
    let (server, client) = lab.exchange(&lab.pending());
    assert!(server.is_err());
    assert!(client.is_err());
    assert_eq!(
        lab.bootstrap(0).access().unwrap().unwrap().generation(),
        before
    );
    assert!(lab.bootstrap(1).witness().unwrap().is_none());
}

#[test]
fn vault_revocation_propagates_permanent_retirement_over_existing_control_repair() {
    let lab = Lab::new(true);
    let (owner, peer) = lab.exchange(&lab.pending());
    owner.unwrap();
    peer.unwrap();
    let peer_bootstrap = lab.bootstrap(1);
    let peer_actor = peer_bootstrap.request().unwrap().binding().actor();
    let old_peer = lab.endpoint(1);
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    let revoked = client::admin_revoke(&lab.paths[0], &admin, "m2", false).unwrap();
    assert!(revoked.retirement.unwrap().changed);
    let (owner, peer) = lab.exchange(&old_peer);
    owner.unwrap();
    peer.unwrap();
    let journal =
        Journal::open_existing(peer_bootstrap.journal_dir(), peer_bootstrap.trust()).unwrap();
    assert!(
        journal
            .constraints()
            .unwrap()
            .check_actor(&peer_actor)
            .is_err()
    );
    assert!(peer_bootstrap.access_in(&journal).is_err());
    drop(journal);
    assert_eq!(
        lab.bootstrap(0).access().unwrap().unwrap().members().len(),
        1
    );
}

#[test]
fn enrollment_preserves_existing_reader_role() {
    use aterm_secrets::commons::roster::{Grant, Role};
    let lab = Lab::new(true);
    let owner = lab.bootstrap(0);
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    let node = admin
        .prepare_commons_rotation(
            &journal,
            vec![Grant::recorded(
                "m1",
                owner.request().unwrap().binding().clone(),
                Role::Reader,
            )],
        )
        .unwrap();
    journal.ingest(node.bytes()).unwrap();
    drop(journal);
    let (server, peer) = lab.exchange(&lab.pending());
    server.unwrap();
    peer.unwrap();
    let access = owner.access().unwrap().unwrap();
    assert_eq!(access.members().len(), 2);
    assert_eq!(access.local().role(), Role::Reader);
}

fn retain_without_reply(lab: &Lab) {
    let peer = lab.bootstrap(1);
    let request = peer.request().unwrap();
    let (mut pending, challenge) =
        Pending::issue(&request, peer.trust().network, peer.trust().authority).unwrap();
    let response = peer.device().answer(&request, &challenge).unwrap();
    let possession = pending.verify(&response).unwrap();
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    let owner = lab.bootstrap(0);
    let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    admin
        .enroll_commons_member(&owner, &mut journal, possession)
        .unwrap();
}

pub(crate) fn enrollment_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    use aterm_secrets::commons::roster::{Grant, Role};
    let lab = Lab::new(action != "NoEvidence");
    let pending = lab.pending();
    let owner = lab.bootstrap(0);
    let peer = lab.bootstrap(1);
    let scenario = match action {
        "Enroll" => 1,
        "LostReply" => {
            retain_without_reply(&lab);
            assert!(peer.witness().unwrap().is_none());
            2
        }
        "Retry" => {
            let (a, b) = lab.exchange(&pending);
            a.unwrap();
            b.unwrap();
            3
        }
        "NoEvidence" => 4,
        "Excluded" => {
            let admin = client::admin_open(&lab.paths[0]).unwrap();
            let mut doc = admin.doc.clone();
            let value = doc
                .entries()
                .into_iter()
                .find(|e| e.name == aterm_secrets::commons::ENTRY)
                .unwrap()
                .value;
            doc.set_entry(
                &aterm_secrets::text::Audience::Hosts(vec!["m1".into()]),
                aterm_secrets::commons::ENTRY,
                value.as_slice(),
            )
            .unwrap();
            client::admin_publish(&lab.paths[0], &admin, &doc).unwrap();
            5
        }
        "Retired" => {
            retain_without_reply(&lab);
            let admin = client::admin_cached(&lab.paths[0]).unwrap();
            let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
            let request = owner.request().unwrap();
            let node = admin
                .prepare_commons_retirement(
                    &journal,
                    vec![Grant::recorded(
                        "m1",
                        request.binding().clone(),
                        Role::Writer,
                    )],
                    &[peer.request().unwrap().binding().actor()],
                )
                .unwrap();
            journal.ingest(node.bytes()).unwrap();
            6
        }
        "Fork" => {
            let admin = client::admin_cached(&lab.paths[0]).unwrap();
            let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
            let head = *owner.access_in(&journal).unwrap().unwrap().head();
            for payload in [b"fork-a", b"fork-b"] {
                let node = admin
                    .seal_commons(
                        owner.trust().network,
                        aterm_secrets::commons::node::Edit::Update {
                            parent: head,
                            hosts: &["m1".into()],
                            payload,
                        },
                    )
                    .unwrap();
                journal.ingest(node.bytes()).unwrap();
            }
            7
        }
        _ => panic!("unknown enrollment action"),
    };
    let before = Journal::open_existing(owner.journal_dir(), owner.trust())
        .unwrap()
        .inventory()
        .count();
    let (a, b) = lab.exchange(&pending);
    let granted = a.is_ok() && b.is_ok();
    let journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    let after = journal.inventory().count();
    let retained = granted && journal.inventory().filter_map(|id| journal.node(id)).any(|n| n.facts().iter().any(|f| matches!(f, Fact::Bind { binding } if binding == peer.request().unwrap().binding())));
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("granted", i64::from(granted));
    next.insert("retained", i64::from(retained));
    next.insert("same", i64::from(before == after));
    next
}

#[test]
fn enrollment_publication_conforms_to_real_network_retries_and_refusals() {
    let model = aterm_models_db::enrollment_commit_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    for action in &model.actions {
        let next = enrollment_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
    }
    let mut mutant = enrollment_transition(&initial, "LostReply");
    mutant.insert("same", 0);
    assert!(!model.check_invariant("RetryKeepsEpoch", &mutant));
    assert!(!model.successors("LostReply", &initial).contains(&mutant));
}

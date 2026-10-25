// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::*;
use super::*;
use crate::commons::{
    ENTRY,
    binding::{Pending, Request},
    bootstrap::{Bootstrap, Descriptor},
    journal::Journal,
    node::{Edit, Node},
};
use aterm_spec::interp::State as ModelState;

struct Lab {
    _root: aterm_tempfile::TempDir,
    paths: Paths,
    server: Option<crate::server::Running>,
    peer: Identity,
}

impl Lab {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let server = crate::server::start(
            std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
            &crate::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: root.path().join("store"),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(200),
                keep_awake: false,
            },
        )
        .unwrap();
        let paths = Paths::for_home(&root.path().join("m1"));
        create(
            &paths,
            &format!("http://{}/v1/alab", server.addr),
            None,
            Some("m1"),
        )
        .unwrap();
        Self {
            _root: root,
            paths,
            server: Some(server),
            peer: Identity::from_secret_bytes([37; 32]),
        }
    }

    fn peer_key(&self) -> String {
        self.peer.recipient().to_bech32()
    }

    fn add(&self, manual: bool, audience: bool) {
        let admin = admin_open(&self.paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m2", &self.peer_key()).unwrap();
        if audience {
            share(&mut doc, &["m1", "m2"]);
        }
        if manual {
            admin_publish_approved(&self.paths, &admin, &doc, &[self.peer_key()]).unwrap();
            if !audience {
                let admin = admin_open(&self.paths).unwrap();
                let mut doc = admin.doc.clone();
                share(&mut doc, &["m1"]);
                admin_publish(&self.paths, &admin, &doc).unwrap();
            }
        } else {
            admin_publish(&self.paths, &admin, &doc).unwrap();
        }
    }

    fn queue(&self, open: bool, valid: bool) {
        let mut admin = admin_open(&self.paths).unwrap();
        if open {
            let mut doc = admin.doc.clone();
            doc.set_open(true).unwrap();
            admin_publish(&self.paths, &admin, &doc).unwrap();
            admin = admin_open(&self.paths).unwrap();
        }
        let password = admin
            .doc
            .join_password()
            .unwrap_or_else(|| SecretBytes::from(&b"wrong-password"[..]));
        let key = join::Key::derive("alab", password.as_slice());
        let proof = if valid {
            key.prove("alab", &self.peer_key(), "m2")
        } else {
            crate::b64::encode_padded(&[0; 32])
        };
        let url = format!("http://{}/v1/alab", self.server.as_ref().unwrap().addr);
        transport::post_join(
            &url,
            &format!(
                "{} m2 {} proof={proof}",
                self.peer_key(),
                join_code(&self.peer.recipient())
            ),
        )
        .unwrap();
        let approved = auto_approve(&self.paths).unwrap();
        assert_eq!(approved.approved.is_empty(), !open && !valid);
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        self.server.take().unwrap().stop();
    }
}

fn share(doc: &mut Document, hosts: &[&str]) {
    let descriptor = admin_keys::control_descriptor(doc).unwrap().unwrap();
    let audience = doc.commons_audience().unwrap().clone();
    doc.unset_entry(&audience, ENTRY).unwrap();
    doc.set_entry(
        &Audience::Hosts(hosts.iter().map(|h| (*h).into()).collect()),
        ENTRY,
        &descriptor.bytes(),
    )
    .unwrap();
}

fn check(lab: &Lab, admin: &Admin, host: &str, foreign: bool) -> bool {
    let bootstrap = Bootstrap::required(&lab.paths).unwrap();
    let mut trust = bootstrap.trust();
    let alternate = foreign.then(|| {
        Node::seal(
            &admin.commons_seed,
            trust.network,
            Edit::Create {
                hosts: &[],
                payload: b"another authenticated genesis",
            },
            &[lab.peer.recipient()],
            0,
        )
        .unwrap()
    });
    if let Some(node) = &alternate {
        trust.genesis = node.id();
    }
    // The foreign context is a distinct journal even under the same authority.
    let foreign_dir = lab.paths.dir.join("foreign-control");
    let journal = if foreign {
        let mut journal = Journal::open(&foreign_dir, trust).unwrap();
        journal.ingest(alternate.as_ref().unwrap().bytes()).unwrap();
        journal
    } else {
        Journal::open_existing(bootstrap.journal_dir(), trust).unwrap()
    };
    let request = if host == "m1" {
        bootstrap.request().unwrap()
    } else {
        Request::create(
            trust.network,
            trust.authority,
            [23; 16],
            [24; 16],
            &lab.peer,
            &[25; 32],
        )
        .unwrap()
    };
    let (mut pending, challenge) =
        Pending::issue(&request, trust.network, trust.authority).unwrap();
    let answer = if host == "m1" {
        bootstrap.device().answer(&request, &challenge).unwrap()
    } else {
        request.answer(&lab.peer, &[25; 32], &challenge).unwrap()
    };
    let approved = admin
        .auto_approve_commons_member(&journal, host, pending.verify(&answer).unwrap())
        .is_ok();
    if foreign
        || admin
            .commons_admission(host)
            .is_some_and(|e| e.method == Method::Open)
    {
        // Without the provenance/genesis gate this otherwise valid authority,
        // audience, candidate and possession would produce a certificate.
        let (mut pending, challenge) =
            Pending::issue(&request, trust.network, trust.authority).unwrap();
        let answer = request.answer(&lab.peer, &[25; 32], &challenge).unwrap();
        assert!(
            admin
                .approve_commons_member(&journal, host, pending.verify(&answer).unwrap())
                .is_ok()
        );
    }
    approved
}

pub(crate) fn admission_transition(state: &ModelState, action: &str) -> ModelState {
    assert_eq!(state["scenario"], 0);
    let lab = Lab::new();
    let (scenario, expected_evidence) = match action {
        "Password" => {
            lab.queue(false, true);
            (1, 2)
        }
        "Manual" => {
            lab.add(true, true);
            (2, 2)
        }
        "OpenClosed" => {
            lab.queue(true, true);
            (3, 1)
        }
        "Removed" | "Readded" => {
            lab.add(true, true);
            let admin = admin_open(&lab.paths).unwrap();
            admin_revoke(&lab.paths, &admin, "m2", false).unwrap();
            if action == "Readded" {
                lab.add(false, true);
            }
            (if action == "Removed" { 4 } else { 5 }, 0)
        }
        "GenericEdit" => {
            lab.add(false, true);
            (6, 0)
        }
        "Rotate" => {
            lab.add(true, true);
            let admin = admin_open(&lab.paths).unwrap();
            admin_rotate(&lab.paths, &admin, &admin.doc).unwrap();
            (7, 2)
        }
        "Rename" => {
            lab.add(true, true);
            let admin = admin_open(&lab.paths).unwrap();
            let mut doc = admin.doc.clone();
            share(&mut doc, &["m1"]);
            doc.remove_machine("m2", false).unwrap();
            doc.add_machine("m100", &lab.peer_key()).unwrap();
            share(&mut doc, &["m1", "m100"]);
            admin_publish(&lab.paths, &admin, &doc).unwrap();
            (8, 2)
        }
        "Excluded" => {
            lab.add(true, false);
            (9, 2)
        }
        "Legacy" => {
            lab.add(true, true);
            let admin = admin_open(&lab.paths).unwrap();
            let bytes = seal(
                &admin.config.name,
                admin.seq() + 1,
                &admin.seed,
                &admin.commons_seed,
                true,
                &admin.doc,
                &admin.handoff,
            )
            .unwrap();
            let parsed = envelope::parse(&bytes).unwrap();
            let mut lockers = parsed.lockers.clone();
            let plain = admin_keys::admin_plaintext_with_admissions(
                &admin.seed,
                &admin.commons_seed,
                true,
                &admin.doc,
                &Ledger::default(),
            );
            let text = plain.as_str().unwrap();
            let text = text.replacen("alab-vault-admin/3", "alab-vault-admin/2", 1);
            let text = text
                .lines()
                .filter(|line| !line.starts_with("admissions: "))
                .collect::<Vec<_>>()
                .join("\n");
            lockers[0].armored = age::armor(
                &age::encrypt(
                    &recipients_of(&admin.doc.admins(), &admin.doc).unwrap(),
                    text.as_bytes(),
                )
                .unwrap(),
            );
            let bytes = envelope::sign(
                &Unsigned {
                    name: parsed.name,
                    seq: parsed.seq,
                    time: parsed.time,
                    handoff: parsed.handoff,
                    lockers,
                },
                &admin.seed,
            )
            .unwrap();
            transport::publish(&admin.config.source, &bytes).unwrap();
            sync(&lab.paths, None).unwrap();
            (10, 0)
        }
        "Draft" => (11, 0),
        "InvalidProof" => {
            lab.queue(false, false);
            (12, 0)
        }
        "ForeignRoot" => {
            lab.add(true, true);
            (13, 2)
        }
        "Founder" => (14, 2),
        _ => panic!("unknown admission action"),
    };
    let mut admin = admin_open(&lab.paths).unwrap();
    if matches!(action, "Password" | "OpenClosed") {
        let shared = matches!(admin.doc.commons_audience(), Some(Audience::Hosts(hosts)) if hosts.iter().any(|host| host == "m2"));
        assert_eq!(
            shared,
            action == "Password",
            "only qualified approval automatically shares the control descriptor"
        );
        let mut doc = admin.doc.clone();
        share(&mut doc, &["m1", "m2"]);
        if action == "OpenClosed" {
            doc.set_open(false).unwrap();
        }
        admin_publish(&lab.paths, &admin, &doc).unwrap();
        admin = admin_open(&lab.paths).unwrap();
    }
    let host = match action {
        "Rename" => "m100",
        "Founder" => "m1",
        _ => "m2",
    };
    if action == "Draft" {
        admin.doc.add_machine("m2", &lab.peer_key()).unwrap();
        share(&mut admin.doc, &["m1", "m2"]);
    }
    let evidence = admin
        .commons_admission(host)
        .map_or(0, |e| if e.method.qualifies() { 2 } else { 1 });
    assert_eq!(evidence, expected_evidence);
    let allowed = check(&lab, &admin, host, action == "ForeignRoot");
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("evidence", evidence);
    next.insert("allowed", i64::from(allowed));
    next
}

#[test]
fn admission_matches_real_approval_publication_and_certificate_paths() {
    let model = aterm_models_db::admission_evidence_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    for action in &model.actions {
        let next = admission_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
    }
    let mut mutant = admission_transition(&initial, "OpenClosed");
    mutant.insert("allowed", 1);
    assert!(!model.successors("OpenClosed", &initial).contains(&mutant));
    assert!(!model.check_invariant("EvidenceRequired", &mutant));
}

#[test]
fn admission_codec_rejects_ambiguous_truncated_future_and_foreign_state() {
    let lab = Lab::new();
    lab.add(true, true);
    let admin = admin_open(&lab.paths).unwrap();
    let bytes = admin.admissions.bytes();
    assert_eq!(Ledger::decode(&bytes).unwrap(), admin.admissions);
    for n in 0..bytes.len() {
        if n != 4 {
            assert!(Ledger::decode(&bytes[..n]).is_err(), "prefix {n}");
        }
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(Ledger::decode(&extra).is_err());
    let mut reordered = bytes.clone();
    let (a, b) = reordered[HEADER..].split_at_mut(RECORD);
    a.swap_with_slice(b);
    assert!(Ledger::decode(&reordered).is_err());
    let mut duplicate = bytes.clone();
    duplicate[HEADER + RECORD..HEADER + 2 * RECORD]
        .copy_from_slice(&bytes[HEADER..HEADER + RECORD]);
    assert!(Ledger::decode(&duplicate).is_err());
    let mut wrong = admin.admissions.clone();
    wrong.trust.as_mut().unwrap().genesis[0] ^= 1;
    assert!(wrong.validate(&admin.doc, admin.seq()).is_err());
    let mut future = admin.admissions.clone();
    future
        .entries
        .get_mut(&lab.peer.recipient().0)
        .unwrap()
        .sequence = admin.seq() + 1;
    assert!(future.validate(&admin.doc, admin.seq()).is_err());
    let evidence = admin.commons_admission("m2").unwrap();
    assert_eq!(evidence.sequence, admin.seq());
    assert_eq!(evidence.method, Method::Manual);
    let mut downgraded = admin.admissions.clone();
    downgraded
        .record(
            &admin.doc,
            &lab.peer_key(),
            Evidence {
                method: Method::Open,
                ..evidence
            },
        )
        .unwrap();
    assert_eq!(downgraded, admin.admissions);
    assert!(Ledger::default().validate(&admin.doc, admin.seq()).is_err());
    let bound_empty = Ledger::default().retained(&admin.doc).unwrap();
    assert!(bound_empty.entries.is_empty());
    bound_empty.validate(&admin.doc, admin.seq()).unwrap();
    assert!(evidence.authored_ms > 0);
    assert_eq!(
        evidence.author,
        machine_identity(&lab.paths).unwrap().recipient()
    );
    assert_eq!(
        admin.commons_admission("m1").unwrap().method,
        Method::Founder
    );
    let descriptor = admin_keys::control_descriptor(&admin.doc).unwrap().unwrap();
    assert_eq!(
        Descriptor::decode(&descriptor.bytes()).unwrap().trust(),
        descriptor.trust()
    );
}

#[test]
fn draft_join_policy_cannot_bypass_the_authenticated_password() {
    let lab = Lab::new();
    lab.queue(false, false);
    let mut admin = admin_open(&lab.paths).unwrap();
    let before = admin.seq();
    admin.doc.set_open(true).unwrap();
    let done = auto_approve_opened(&lab.paths, &admin).unwrap();
    assert!(done.approved.is_empty());
    assert_eq!(done.refused, [lab.peer_key()]);
    let after = admin_open(&lab.paths).unwrap();
    assert_eq!(after.seq(), before);
    assert!(after.commons_admission("m2").is_none());
}

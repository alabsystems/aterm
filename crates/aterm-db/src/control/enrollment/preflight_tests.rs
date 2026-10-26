// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{tests::Lab, *};
use aterm_secrets::{
    commons::roster::{Grant, Role},
    text::Audience,
};
use std::{cell::Cell, io::Cursor};

/// The real responder's byte stream. On its first read, answer the actual
/// encrypted challenge, then change local policy before delivering that reply.
struct Exchange<'a> {
    peer: &'a Bootstrap,
    request: Request,
    output: Vec<u8>,
    input: Option<Cursor<Vec<u8>>>,
    change: Option<Box<dyn FnOnce() + 'a>>,
    corrupt: bool,
}

impl Exchange<'_> {
    fn frames(&self) -> Vec<Vec<u8>> {
        let mut reader = Cursor::new(self.output.as_slice());
        let mut frames = Vec::new();
        while reader.position() < self.output.len() as u64 {
            frames.push(receive_frame(&mut reader, MAX_HELLO).unwrap());
        }
        frames
    }
}

impl Write for Exchange<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.output.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for Exchange<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if self.input.is_none() {
            let frames = self.frames();
            assert_eq!(frames.len(), 2, "hello and challenge must precede the read");
            assert!(frames[0].starts_with(b"ACX1"));
            let mut response = self
                .peer
                .device()
                .answer(&self.request, &frames[1])
                .unwrap();
            if self.corrupt {
                *response.last_mut().unwrap() ^= 1;
            }
            if let Some(change) = self.change.take() {
                change();
            }
            let mut wire = Vec::new();
            send_frame(&mut wire, &response).unwrap();
            self.input = Some(Cursor::new(wire));
        }
        self.input.as_mut().unwrap().read(bytes)
    }
}

fn count(lab: &Lab) -> usize {
    let b = lab.bootstrap(0);
    Journal::open_existing(b.journal_dir(), b.trust())
        .unwrap()
        .inventory()
        .count()
}

fn change(lab: &Lab, action: &str) {
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    match action {
        "ExcludedBefore" | "ExcludedDuring" => {
            let mut doc = admin.doc.clone();
            let value = doc
                .entries()
                .into_iter()
                .find(|e| e.name == aterm_secrets::commons::ENTRY)
                .unwrap()
                .value;
            doc.set_entry(
                &Audience::Hosts(vec!["m1".into()]),
                aterm_secrets::commons::ENTRY,
                value.as_slice(),
            )
            .unwrap();
            client::admin_publish(&lab.paths[0], &admin, &doc).unwrap();
        }
        "RetiredBefore" | "RetiredDuring" => {
            let b = lab.bootstrap(0);
            let mut journal = Journal::open_existing(b.journal_dir(), b.trust()).unwrap();
            let node = admin
                .prepare_commons_retirement(
                    &journal,
                    vec![Grant::recorded(
                        "m1",
                        b.request().unwrap().binding().clone(),
                        Role::Writer,
                    )],
                    &[lab.bootstrap(1).request().unwrap().binding().actor()],
                )
                .unwrap();
            journal.ingest(node.bytes()).unwrap();
        }
        "ForkBefore" | "ForkDuring" => {
            let b = lab.bootstrap(0);
            let mut journal = Journal::open_existing(b.journal_dir(), b.trust()).unwrap();
            let head = *b.access_in(&journal).unwrap().unwrap().head();
            for payload in [b"fork-a", b"fork-b"] {
                let node = admin
                    .seal_commons(
                        b.trust().network,
                        aterm_secrets::commons::node::Edit::Update {
                            parent: head,
                            hosts: &["m1".into()],
                            payload,
                        },
                    )
                    .unwrap();
                journal.ingest(node.bytes()).unwrap();
            }
        }
        _ => {}
    }
}

pub(crate) fn gate_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let lab = Lab::new(action != "NoEvidence");
    let pending = lab.pending();
    if action.starts_with("Retired") {
        let (a, b) = lab.exchange(&pending);
        a.unwrap();
        b.unwrap();
    }
    let endpoint = lab.endpoint(0);
    let peer = lab.bootstrap(1);
    if action.ends_with("Before") {
        change(&lab, action);
    }
    let baseline = Cell::new(count(&lab));
    let mut stream = Exchange {
        peer: &peer,
        request: peer.request().unwrap(),
        output: Vec::new(),
        input: None,
        change: Some(Box::new(|| {
            if action.ends_with("During") {
                change(&lab, action);
            }
            baseline.set(count(&lab));
        })),
        corrupt: action == "InvalidProof",
    };
    let proof = NodeProof {
        node: pending.identity.node().into(),
        key: pending.identity.key(),
    };
    let result = serve(
        &endpoint,
        &mut stream,
        Some(&proof),
        &pending.hello,
        &mut Repair::default(),
    );
    let frames = stream.frames();
    let challenged = frames.len() >= 2;
    let granted = result.is_ok();
    assert_eq!(
        frames.len(),
        if granted {
            3
        } else if challenged {
            2
        } else {
            0
        }
    );
    if granted {
        check_witness(endpoint.trust, &stream.request, &frames[2]).unwrap();
        let owner = lab.bootstrap(0);
        let journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
        let node =
            Node::verify(&frames[2], owner.trust().network, owner.trust().authority).unwrap();
        assert_eq!(
            journal.node(&node.id()).map(Node::bytes),
            Some(frames[2].as_slice()),
            "reply was not durably retained"
        );
    }
    let mut next = state.clone();
    next.insert(
        "scenario",
        match action {
            "Eligible" => 1,
            "NoEvidence" => 2,
            "ExcludedBefore" => 3,
            "RetiredBefore" => 4,
            "ForkBefore" => 5,
            "ExcludedDuring" => 6,
            "RetiredDuring" => 7,
            "ForkDuring" => 8,
            "InvalidProof" => 9,
            _ => panic!("unknown gate action"),
        },
    );
    next.insert("challenged", i64::from(challenged));
    next.insert("granted", i64::from(granted));
    next.insert("retained", (count(&lab) - baseline.get()) as i64);
    next
}

#[test]
fn preflight_and_post_challenge_policy_conform_with_negative_controls() {
    let model = aterm_models_db::enrollment_gate_model();
    aterm_spec::interp::prove_and_catch(&model);
    let initial = model.init_state();
    for action in &model.actions {
        let actual = gate_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&actual),
            "{}: {actual:?}",
            action.name
        );
    }
    let mut early = gate_transition(&initial, "NoEvidence");
    early.insert("challenged", 1);
    assert!(!model.check_invariant("QualifiedChallenge", &early));
    let mut stale = gate_transition(&initial, "ExcludedDuring");
    stale.insert("granted", 1);
    assert!(!model.check_invariant("NoGrantAfterRefusal", &stale));
}

#[test]
fn an_excluded_existing_member_blocks_a_new_candidate_before_challenge() {
    use aterm_secrets::client::{JoinOptions, Paths};
    let lab = Lab::new(true);
    let (a, b) = lab.exchange(&lab.pending());
    a.unwrap();
    b.unwrap();
    let third = Paths::for_home(&lab.root.path().join("m3"));
    let recipient = client::machine_identity(&third)
        .unwrap()
        .recipient()
        .to_bech32();
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_machine("m3", &recipient).unwrap();
    client::admin_publish_approved(&lab.paths[0], &admin, &doc, &[recipient]).unwrap();
    client::join(
        &third,
        &JoinOptions {
            source: format!("{}#{}", lab.source.display(), lab.signer),
            host: Some("m3".into()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: aterm_secrets::client::JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .unwrap();
    let peer = Bootstrap::required(&third).unwrap();
    let pending = Endpoint::pending_from_bootstrap(&peer, third).unwrap();
    let endpoint = lab.endpoint(0);
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    let value = doc
        .entries()
        .into_iter()
        .find(|e| e.name == aterm_secrets::commons::ENTRY)
        .unwrap()
        .value;
    doc.set_entry(
        &Audience::Hosts(vec!["m1".into(), "m3".into()]),
        aterm_secrets::commons::ENTRY,
        value.as_slice(),
    )
    .unwrap();
    client::admin_publish(&lab.paths[0], &admin, &doc).unwrap();
    let proof = NodeProof {
        node: pending.identity.node().into(),
        key: pending.identity.key(),
    };
    let mut stream = Exchange {
        peer: &peer,
        request: peer.request().unwrap(),
        output: Vec::new(),
        input: None,
        change: None,
        corrupt: false,
    };
    let before = count(&lab);
    assert!(
        serve(
            &endpoint,
            &mut stream,
            Some(&proof),
            &pending.hello,
            &mut Repair::default()
        )
        .is_err()
    );
    assert!(
        stream.output.is_empty(),
        "an excluded member was only checked after the challenge"
    );
    assert_eq!(count(&lab), before);
}

#[test]
fn unavailable_authority_custody_sends_no_protocol_response() {
    let lab = Lab::new(true);
    let peer = lab.bootstrap(1);
    let pending = lab.pending();
    let endpoint = lab.endpoint(0);
    let proof = NodeProof {
        node: pending.identity.node().into(),
        key: pending.identity.key(),
    };
    std::fs::rename(&lab.paths[0].dir, lab.root.path().join("unavailable-owner")).unwrap();
    let mut stream = Exchange {
        peer: &peer,
        request: peer.request().unwrap(),
        output: Vec::new(),
        input: None,
        change: None,
        corrupt: false,
    };
    assert!(
        serve(
            &endpoint,
            &mut stream,
            Some(&proof),
            &pending.hello,
            &mut Repair::default()
        )
        .is_err()
    );
    assert!(
        stream.output.is_empty(),
        "missing authority was only checked after the challenge"
    );
    assert!(
        !lab.paths[0].dir.exists(),
        "preflight recreated missing custody"
    );
}

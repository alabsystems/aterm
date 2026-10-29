// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::node::{Edit, MAX_NODE, MAX_PAYLOAD};

struct Fixture {
    identity: Identity,
    nodes: [Node; 5],
    trust: Trust,
}
impl Fixture {
    fn new() -> Self {
        let identity = Identity::from_secret_bytes([17; 32]);
        let recipients = [identity.recipient()];
        let seed = [23; 32];
        let network = [29; 16];
        let hosts = ["m1".to_string()];
        let create = Node::seal(
            &seed,
            network,
            Edit::Create {
                hosts: &hosts,
                payload: b"initial member secret",
            },
            &recipients,
            u64::MAX,
        )
        .unwrap();
        let left = Node::seal(
            &seed,
            network,
            Edit::Update {
                parent: create.id(),
                hosts: &hosts,
                payload: b"left member secret",
            },
            &recipients,
            20,
        )
        .unwrap();
        let right = Node::seal(
            &seed,
            network,
            Edit::Update {
                parent: create.id(),
                hosts: &hosts,
                payload: b"right member secret",
            },
            &recipients,
            10,
        )
        .unwrap();
        let tail = Node::seal(
            &seed,
            network,
            Edit::Update {
                parent: right.id(),
                hosts: &hosts,
                payload: b"tail member secret",
            },
            &recipients,
            0,
        )
        .unwrap();
        let recovery = Node::seal(
            &seed,
            network,
            Edit::Recover {
                heads: &[left.id(), tail.id()],
                selected: left.id(),
            },
            &[],
            5,
        )
        .unwrap();
        let trust = Trust {
            network,
            authority: create.authority(),
            genesis: create.id(),
        };
        Self {
            identity,
            nodes: [create, left, right, tail, recovery],
            trust,
        }
    }
    fn visible(&self, journal: &Journal) -> i64 {
        match journal.state() {
            State::Ready { head, .. } => {
                self.nodes
                    .iter()
                    .position(|node| node.id() == head)
                    .unwrap() as i64
                    + 1
            }
            _ => 0,
        }
    }
}
fn tmp() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("commons-journal-")
        .tempdir()
        .unwrap()
}

#[test]
fn control_writer_contention_is_busy_and_release_preserves_the_journal() {
    let fixture = Fixture::new();
    let dir = tmp();
    let mut owner = Journal::open(dir.path(), fixture.trust).unwrap();
    owner.ingest(fixture.nodes[0].bytes()).unwrap();
    let error = Journal::open_existing(dir.path(), fixture.trust)
        .err()
        .unwrap();
    assert_eq!(error.kind(), crate::ErrorKind::JournalBusy);
    assert!(owner.node(&fixture.nodes[0].id()).is_some());
    drop(owner);
    let successor = Journal::open_existing(dir.path(), fixture.trust).unwrap();
    assert!(successor.node(&fixture.nodes[0].id()).is_some());
    drop(successor);
    let mut wrong = fixture.trust;
    wrong.network[0] ^= 1;
    let error = Journal::open_existing(dir.path(), wrong).err().unwrap();
    assert_eq!(error.kind(), crate::ErrorKind::Other);
}

#[test]
fn unrelated_would_block_is_not_control_journal_contention() {
    let error = crate::Error::from(std::io::Error::from(std::io::ErrorKind::WouldBlock));
    assert_eq!(error.kind(), crate::ErrorKind::Other);
}

/// Actual signed, durably appended transition projector used by the model's
/// source anchors. Reopen also checks replay, independent of the live index.
fn journal_transition(
    fixture: &Fixture,
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let root = tmp();
    let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
    journal.ingest(fixture.nodes[0].bytes()).unwrap();
    let names = ["left", "right", "tail", "recovery"];
    // Deliberately deliver children before parents; the state is a set.
    for index in (0..names.len()).rev() {
        if state[names[index]] == 1 {
            journal.ingest(fixture.nodes[index + 1].bytes()).unwrap();
        }
    }
    assert_eq!(fixture.visible(&journal), state["visible"]);
    let index = match action {
        "ReceiveLeft" => 0,
        "ReceiveRight" => 1,
        "ReceiveTail" => 2,
        "ReceiveRecovery" => 3,
        other => panic!("unknown action {other}"),
    };
    journal.ingest(fixture.nodes[index + 1].bytes()).unwrap();
    let live = journal.state();
    drop(journal);
    let journal = Journal::open(root.path(), fixture.trust).unwrap();
    assert_eq!(journal.state(), live);
    let mut next = state.clone();
    next.insert(names[index], 1);
    next.insert("visible", fixture.visible(&journal));
    next
}

#[test]
fn every_reachable_delivery_transition_conforms_and_unsafe_winners_fail() {
    let fixture = Fixture::new();
    let model = aterm_models_db::membership_journal_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = journal_transition(&fixture, &state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let before = BTreeMap::from([
        ("left", 1),
        ("right", 0),
        ("tail", 0),
        ("recovery", 0),
        ("visible", 2),
    ]);
    let mut bad = journal_transition(&fixture, &before, "ReceiveRight");
    bad.insert("visible", 3); // Unsafe last-arrival winner.
    assert!(!model.successors("ReceiveRight", &before).contains(&bad));
    assert!(!model.check_invariant("ForkHasNoWinner", &bad));
    let before = BTreeMap::from([
        ("left", 0),
        ("right", 0),
        ("tail", 0),
        ("recovery", 0),
        ("visible", 1),
    ]);
    let mut bad = journal_transition(&fixture, &before, "ReceiveRecovery");
    bad.insert("visible", 5); // Unsafe unverified ancestry.
    assert!(!model.successors("ReceiveRecovery", &before).contains(&bad));
    assert!(!model.check_invariant("RecoveryHasAncestry", &bad));
}

#[test]
fn offline_repair_deduplicates_and_late_forks_withdraw_readiness_after_restart() {
    let fixture = Fixture::new();
    let root = tmp();
    let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
    assert_eq!(journal.state(), State::Empty);
    assert!(journal.current(&fixture.identity).unwrap().is_none());
    let receipt = journal.ingest(fixture.nodes[3].bytes()).unwrap();
    assert_eq!(journal.state(), State::Pending);
    assert_eq!(journal.ingest(fixture.nodes[3].bytes()).unwrap(), receipt);
    journal.ingest(fixture.nodes[2].bytes()).unwrap();
    assert_eq!(journal.state(), State::Pending);
    journal.ingest(fixture.nodes[0].bytes()).unwrap();
    assert_eq!(fixture.visible(&journal), 4);
    assert_eq!(
        journal
            .current(&fixture.identity)
            .unwrap()
            .unwrap()
            .payload
            .as_slice(),
        b"tail member secret"
    );
    assert!(fixture.nodes[0].authored_ms() > fixture.nodes[3].authored_ms());
    drop(journal);
    let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
    assert_eq!(journal.receipt(&fixture.nodes[3].id()), Some(receipt));
    journal.ingest(fixture.nodes[1].bytes()).unwrap();
    assert!(matches!(journal.state(), State::Conflict { .. }));
    assert!(journal.current(&fixture.identity).unwrap().is_none());
    journal.ingest(fixture.nodes[4].bytes()).unwrap();
    let current = journal.current(&fixture.identity).unwrap().unwrap();
    assert_eq!(current.head, fixture.nodes[4].id());
    assert_eq!(current.basis, fixture.nodes[1].id());
    assert_eq!(current.payload.as_slice(), b"left member secret");
    assert_eq!(journal.inventory().count(), 5);
    drop(journal);
    let journal = Journal::open(root.path(), fixture.trust).unwrap();
    assert_eq!(
        journal.state(),
        State::Ready {
            head: current.head,
            basis: current.basis
        }
    );
    assert!(
        journal
            .current(&Identity::from_secret_bytes([99; 32]))
            .is_err()
    );
    let disk = std::fs::read(root.path().join("control.astream")).unwrap();
    assert!(
        !disk
            .windows(b"member secret".len())
            .any(|bytes| bytes == b"member secret")
    );
}

#[test]
fn authentication_and_exact_anchor_precede_any_append() {
    let fixture = Fixture::new();
    let root = tmp();
    let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
    let node = &fixture.nodes[0];
    for index in [0, 5, 25, 54, node.bytes().len() - 1] {
        let mut bytes = node.bytes().to_vec();
        bytes[index] ^= 1;
        assert!(journal.ingest(&bytes).is_err());
    }
    assert!(journal.ingest(&vec![0; MAX_NODE + 1]).is_err());
    assert!(Node::verify(node.bytes(), [99; 16], fixture.trust.authority).is_err());
    assert!(Node::verify(node.bytes(), fixture.trust.network, [99; 32]).is_err());
    let foreign_genesis = Node::seal(
        &[23; 32],
        fixture.trust.network,
        Edit::Create {
            hosts: &[],
            payload: b"different genesis",
        },
        &[fixture.identity.recipient()],
        0,
    )
    .unwrap();
    assert!(journal.ingest(foreign_genesis.bytes()).is_err());
    assert_eq!(journal.state(), State::Empty);
    assert_eq!(journal.inventory().count(), 0);
    assert!(
        Journal::open(root.path(), fixture.trust).is_err(),
        "exclusive local writer"
    );
    journal.ingest(node.bytes()).unwrap();
    drop(journal);
    let mut changed = fixture.trust;
    changed.genesis = foreign_genesis.id();
    assert!(Journal::open(root.path(), changed).is_err());
    changed = fixture.trust;
    changed.authority[0] ^= 1;
    assert!(Journal::open(root.path(), changed).is_err());
    assert_eq!(
        Journal::open(root.path(), fixture.trust)
            .unwrap()
            .inventory()
            .count(),
        1
    );
    let anchor = root.path().join("control.anchor");
    std::fs::remove_file(&anchor).unwrap();
    assert!(Journal::open(root.path(), changed).is_err());
    assert!(
        !anchor.exists(),
        "failed replay must not install another anchor"
    );
    assert_eq!(
        Journal::open(root.path(), fixture.trust)
            .unwrap()
            .inventory()
            .count(),
        1
    );
}

#[test]
fn recovery_must_select_a_named_existing_parent_and_keeps_ciphertext_private() {
    let fixture = Fixture::new();
    let seed = [23; 32];
    let network = fixture.trust.network;
    assert!(
        Node::seal(
            &seed,
            network,
            Edit::Recover {
                heads: &[fixture.nodes[1].id()],
                selected: fixture.nodes[1].id()
            },
            &[],
            0
        )
        .is_err()
    );
    assert!(
        Node::seal(
            &seed,
            network,
            Edit::Recover {
                heads: &[fixture.nodes[1].id(), fixture.nodes[1].id()],
                selected: fixture.nodes[1].id()
            },
            &[],
            0
        )
        .is_err()
    );
    assert!(
        Node::seal(
            &seed,
            network,
            Edit::Recover {
                heads: &[fixture.nodes[1].id(), fixture.nodes[2].id()],
                selected: fixture.nodes[0].id()
            },
            &[],
            0
        )
        .is_err()
    );
    assert!(
        Node::seal(
            &seed,
            network,
            Edit::Create {
                hosts: &[],
                payload: b"no recipients"
            },
            &[],
            0
        )
        .is_err()
    );
    assert!(
        Node::seal(
            &seed,
            network,
            Edit::Create {
                hosts: &[],
                payload: &vec![0; MAX_PAYLOAD + 1]
            },
            &[fixture.identity.recipient()],
            0
        )
        .is_err()
    );
    assert!(fixture.nodes[4].open(&fixture.identity).is_err());
    let removed = Identity::from_secret_bytes([101; 32]);
    let retained = Identity::from_secret_bytes([102; 32]);
    let update = Node::seal(
        &seed,
        network,
        Edit::Update {
            parent: fixture.nodes[0].id(),
            hosts: &[],
            payload: b"rotated control key",
        },
        &[retained.recipient()],
        0,
    )
    .unwrap();
    assert!(update.open(&removed).is_err());
    assert_eq!(
        update.open(&retained).unwrap().as_slice(),
        b"rotated control key"
    );
}

#[test]
fn independently_received_branches_converge_by_ciphertext_set_union() {
    let fixture = Fixture::new();
    let left_root = tmp();
    let right_root = tmp();
    let mut left = Journal::open(left_root.path(), fixture.trust).unwrap();
    let mut right = Journal::open(right_root.path(), fixture.trust).unwrap();
    for journal in [&mut left, &mut right] {
        journal.ingest(fixture.nodes[0].bytes()).unwrap();
    }
    left.ingest(fixture.nodes[1].bytes()).unwrap();
    right.ingest(fixture.nodes[2].bytes()).unwrap();
    right.ingest(fixture.nodes[3].bytes()).unwrap();
    let left_bytes: Vec<_> = left
        .inventory()
        .map(|id| left.node(id).unwrap().bytes().to_vec())
        .collect();
    let right_bytes: Vec<_> = right
        .inventory()
        .map(|id| right.node(id).unwrap().bytes().to_vec())
        .collect();
    for bytes in &left_bytes {
        right.ingest(bytes).unwrap();
    }
    for bytes in right_bytes.iter().rev() {
        left.ingest(bytes).unwrap();
    }
    assert_eq!(left.state(), right.state());
    assert!(matches!(left.state(), State::Conflict { .. }));
    left.ingest(fixture.nodes[4].bytes()).unwrap();
    right.ingest(fixture.nodes[4].bytes()).unwrap();
    assert_eq!(left.state(), right.state());
    assert_eq!(
        left.current(&fixture.identity).unwrap().unwrap().payload,
        right.current(&fixture.identity).unwrap().unwrap().payload
    );
}

#[test]
fn recovery_does_not_hide_late_forks_and_can_itself_be_selected() {
    let fixture = Fixture::new();
    let root = tmp();
    let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
    for node in &fixture.nodes {
        journal.ingest(node.bytes()).unwrap();
    }
    assert_eq!(fixture.visible(&journal), 5);
    let late = Node::seal(
        &[23; 32],
        fixture.trust.network,
        Edit::Update {
            parent: fixture.nodes[0].id(),
            hosts: &[],
            payload: b"late branch",
        },
        &[fixture.identity.recipient()],
        u64::MAX,
    )
    .unwrap();
    journal.ingest(late.bytes()).unwrap();
    assert!(matches!(journal.state(), State::Conflict { .. }));
    let repair = Node::seal(
        &[23; 32],
        fixture.trust.network,
        Edit::Recover {
            heads: &[fixture.nodes[4].id(), late.id()],
            selected: fixture.nodes[4].id(),
        },
        &[],
        0,
    )
    .unwrap();
    journal.ingest(repair.bytes()).unwrap();
    assert_eq!(
        journal.state(),
        State::Ready {
            head: repair.id(),
            basis: fixture.nodes[1].id()
        }
    );
    assert_eq!(
        journal
            .current(&fixture.identity)
            .unwrap()
            .unwrap()
            .payload
            .as_slice(),
        b"left member secret"
    );
}

#[test]
fn comparable_recovery_parents_are_a_durable_fault_in_either_delivery_order() {
    let fixture = Fixture::new();
    let false_recovery = Node::seal(
        &[23; 32],
        fixture.trust.network,
        Edit::Recover {
            heads: &[fixture.nodes[0].id(), fixture.nodes[1].id()],
            selected: fixture.nodes[0].id(),
        },
        &[],
        0,
    )
    .unwrap();
    for first in [true, false] {
        let root = tmp();
        let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
        if first {
            journal.ingest(false_recovery.bytes()).unwrap();
        }
        for node in &fixture.nodes[..2] {
            journal.ingest(node.bytes()).unwrap();
        }
        if !first {
            journal.ingest(false_recovery.bytes()).unwrap();
        }
        let fault = State::Invalid {
            nodes: vec![false_recovery.id()],
        };
        assert_eq!(journal.state(), fault);
        assert!(journal.current(&fixture.identity).unwrap().is_none());
        let orphan = Node::seal(
            &[23; 32],
            fixture.trust.network,
            Edit::Update {
                parent: [88; 32],
                hosts: &[],
                payload: b"orphan",
            },
            &[fixture.identity.recipient()],
            0,
        )
        .unwrap();
        journal.ingest(orphan.bytes()).unwrap();
        assert_eq!(
            journal.state(),
            fault,
            "missing ancestry cannot hide an established fault"
        );
        drop(journal);
        assert_eq!(
            Journal::open(root.path(), fixture.trust).unwrap().state(),
            fault
        );
    }
}

#[test]
fn cascaded_recovery_handles_more_heads_than_one_node_can_name() {
    let fixture = Fixture::new();
    let root = tmp();
    let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
    journal.ingest(fixture.nodes[0].bytes()).unwrap();
    let mut heads = Vec::new();
    for index in 0..65_u64 {
        let node = Node::seal(
            &[23; 32],
            fixture.trust.network,
            Edit::Update {
                parent: fixture.nodes[0].id(),
                hosts: &[],
                payload: &index.to_le_bytes(),
            },
            &[fixture.identity.recipient()],
            index,
        )
        .unwrap();
        journal.ingest(node.bytes()).unwrap();
        heads.push(node.id());
    }
    assert!(matches!(journal.state(), State::Conflict { heads: known } if known.len() == 65));
    let selected = heads[0];
    assert!(
        Node::seal(
            &[23; 32],
            fixture.trust.network,
            Edit::Recover {
                heads: &heads,
                selected
            },
            &[],
            0
        )
        .is_err()
    );
    let first = Node::seal(
        &[23; 32],
        fixture.trust.network,
        Edit::Recover {
            heads: &heads[..64],
            selected,
        },
        &[],
        0,
    )
    .unwrap();
    journal.ingest(first.bytes()).unwrap();
    assert!(matches!(journal.state(), State::Conflict { heads: known } if known.len() == 2));
    let final_node = Node::seal(
        &[23; 32],
        fixture.trust.network,
        Edit::Recover {
            heads: &[first.id(), heads[64]],
            selected: first.id(),
        },
        &[],
        0,
    )
    .unwrap();
    journal.ingest(final_node.bytes()).unwrap();
    assert_eq!(
        journal.state(),
        State::Ready {
            head: final_node.id(),
            basis: selected
        }
    );
}

#[test]
fn corrupt_committed_ciphertext_is_refused_without_repair_or_cleartext_fallback() {
    let fixture = Fixture::new();
    let root = tmp();
    let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
    journal.ingest(fixture.nodes[0].bytes()).unwrap();
    drop(journal);
    let path = root.path().join("control.astream");
    let mut disk = std::fs::read(&path).unwrap();
    let needle = fixture.nodes[0].bytes();
    let start = disk
        .windows(needle.len())
        .position(|bytes| bytes == needle)
        .unwrap();
    disk[start + needle.len() / 2] ^= 1;
    std::fs::write(&path, &disk).unwrap();
    assert!(Journal::open(root.path(), fixture.trust).is_err());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        disk,
        "opening must not erase evidence"
    );
}

#[test]
fn capacity_refusal_keeps_acknowledged_history_and_the_same_bytes_are_retryable() {
    let fixture = Fixture::new();
    let root = tmp();
    let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
    Arc::make_mut(&mut journal.view).index = Index::new(1);
    let receipt = journal.ingest(fixture.nodes[0].bytes()).unwrap();
    assert!(journal.ingest(fixture.nodes[1].bytes()).is_err());
    assert_eq!(journal.ingest(fixture.nodes[0].bytes()).unwrap(), receipt);
    assert_eq!(journal.inventory().count(), 1);
    assert_eq!(
        journal
            .current(&fixture.identity)
            .unwrap()
            .unwrap()
            .payload
            .as_slice(),
        b"initial member secret"
    );
    drop(journal);
    let journal = Journal::open(root.path(), fixture.trust).unwrap();
    assert_eq!(journal.inventory().count(), 1);
    assert_eq!(journal.receipt(&fixture.nodes[0].id()), Some(receipt));
}

#[test]
fn reverse_repair_reserves_leaves_instead_of_every_pending_ancestor() {
    fn append(headers: &mut Vec<astream_wire::sync::Header>, parents: Vec<Digest>) -> Digest {
        let mut id = [0; 32];
        id[..8].copy_from_slice(&(headers.len() as u64 + 1).to_be_bytes());
        let mut actor = [0; 48];
        actor[..32].copy_from_slice(&id);
        headers.push(astream_wire::sync::Header {
            id,
            actor,
            sequence: 1,
            previous: None,
            parents,
        });
        id
    }
    let mut headers = Vec::new();
    let genesis = append(&mut headers, vec![]);
    let mut head = genesis;
    for _ in 0..4 {
        let left = append(&mut headers, vec![head]);
        let right = append(&mut headers, vec![head]);
        head = append(&mut headers, vec![left, right]);
    }
    // Normal data exactly fills its allowance; four recovery records are legal
    // extra records. Their ancestor chains arrive before genesis in this repair.
    while headers.len() < MAX_NODES - RECOVERY_NODES + 4 {
        head = append(&mut headers, vec![head]);
    }
    let mut index = Index::new(MAX_NODES);
    for (n, header) in headers.iter().rev().enumerate() {
        index.insert(header.clone()).unwrap();
        let frontier = repair_frontier(&index, genesis);
        assert!(frontier <= 3);
        let required = frontier
            .saturating_sub(1)
            .div_ceil(astream_wire::sync::MAX_PARENTS - 1);
        assert!(n + 1 + required <= MAX_NODES);
    }
    assert_eq!(index.project().heads, BTreeSet::from([head]));
    assert!(!related_parents(&index, &headers[3].parents));
    assert!(related_parents(&index, &[genesis, head]));

    let fixture = Fixture::new();
    assert_eq!(
        data_usage(fixture.nodes.iter()),
        data_usage(fixture.nodes[..4].iter())
    );
    assert_eq!(data_usage(fixture.nodes.iter()).0, 4);
}

#[test]
fn truncated_or_unpinned_history_cannot_silently_restore_old_membership() {
    let fixture = Fixture::new();
    let root = tmp();
    let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
    journal.ingest(fixture.nodes[0].bytes()).unwrap();
    let path = root.path().join("control.astream");
    let old = std::fs::read(&path).unwrap();
    journal.ingest(fixture.nodes[1].bytes()).unwrap();
    drop(journal);
    std::fs::write(&path, old).unwrap();
    assert!(Journal::open(root.path(), fixture.trust).is_err());
    assert!(
        Journal::open(root.path(), fixture.trust).is_err(),
        "loss remains fenced after another open"
    );

    let root = tmp();
    let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
    journal.ingest(fixture.nodes[0].bytes()).unwrap();
    drop(journal);
    std::fs::remove_file(root.path().join("control.checkpoint")).unwrap();
    assert!(Journal::open(root.path(), fixture.trust).is_err());
}

#[test]
fn a_peer_can_repair_ciphertext_ancestry_it_was_not_allowed_to_decrypt() {
    let fixture = Fixture::new();
    let excluded = Node::seal(
        &[23; 32],
        fixture.trust.network,
        Edit::Update {
            parent: fixture.nodes[0].id(),
            hosts: &[],
            payload: b"another audience",
        },
        &[Identity::from_secret_bytes([123; 32]).recipient()],
        0,
    )
    .unwrap();
    assert!(excluded.open(&fixture.identity).is_err());
    let recovery = Node::seal(
        &[23; 32],
        fixture.trust.network,
        Edit::Recover {
            heads: &[fixture.nodes[1].id(), excluded.id()],
            selected: fixture.nodes[1].id(),
        },
        &[],
        0,
    )
    .unwrap();
    let root = tmp();
    let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
    // Recovery arrives first, and opening the excluded ancestor is unnecessary.
    for node in [&recovery, &excluded, &fixture.nodes[1], &fixture.nodes[0]] {
        journal.ingest(node.bytes()).unwrap();
    }
    assert_eq!(
        journal
            .current(&fixture.identity)
            .unwrap()
            .unwrap()
            .payload
            .as_slice(),
        b"left member secret"
    );
    assert!(
        journal
            .node(&excluded.id())
            .unwrap()
            .open(&fixture.identity)
            .is_err()
    );
}

#[test]
fn a_failed_checkpoint_withholds_acknowledgement_and_retries_exact_bytes_after_reopen() {
    let fixture = Fixture::new();
    let root = tmp();
    let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
    journal.ingest(fixture.nodes[0].bytes()).unwrap();
    let checkpoint = root.path().join("control.checkpoint");
    let backup = root.path().join("checkpoint-before-fault");
    std::fs::rename(&checkpoint, &backup).unwrap();
    std::fs::create_dir(&checkpoint).unwrap();
    assert!(journal.ingest(fixture.nodes[1].bytes()).is_err());
    assert_eq!(journal.state(), State::Uncertain);
    assert!(journal.current(&fixture.identity).unwrap().is_none());
    assert!(journal.ingest(fixture.nodes[0].bytes()).is_err());
    drop(journal);
    std::fs::remove_dir(&checkpoint).unwrap();
    std::fs::rename(&backup, &checkpoint).unwrap();
    let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
    assert_eq!(
        journal.inventory().count(),
        1,
        "unacknowledged bytes are not promoted by open"
    );
    let receipt = journal.ingest(fixture.nodes[1].bytes()).unwrap();
    assert_eq!(journal.inventory().count(), 2);
    assert_eq!(journal.receipt(&fixture.nodes[1].id()), Some(receipt));
    assert_eq!(
        journal.state(),
        State::Ready {
            head: fixture.nodes[1].id(),
            basis: fixture.nodes[1].id()
        }
    );
}

#[test]
fn a_torn_unacknowledged_suffix_recovers_without_erasing_acknowledged_history() {
    use std::io::Write;
    let fixture = Fixture::new();
    let root = tmp();
    let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
    journal.ingest(fixture.nodes[0].bytes()).unwrap();
    drop(journal);
    let path = root.path().join("control.astream");
    let before = std::fs::read(&path).unwrap();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&[0xa5, 0x71]).unwrap();
    aterm_durable::durable(&file).unwrap();
    drop(file);
    let journal = Journal::open(root.path(), fixture.trust).unwrap();
    assert_eq!(
        journal.state(),
        State::Ready {
            head: fixture.nodes[0].id(),
            basis: fixture.nodes[0].id()
        }
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(journal);
    assert_eq!(
        Journal::open(root.path(), fixture.trust)
            .unwrap()
            .inventory()
            .count(),
        1
    );
}

#[test]
fn an_existing_anchor_never_treats_lost_history_as_a_fresh_empty_journal() {
    let fixture = Fixture::new();
    for keep_empty_file in [false, true] {
        let root = tmp();
        let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
        journal.ingest(fixture.nodes[0].bytes()).unwrap();
        journal.ingest(fixture.nodes[1].bytes()).unwrap();
        drop(journal);
        let path = root.path().join("control.astream");
        std::fs::remove_file(root.path().join("control.checkpoint")).unwrap();
        if keep_empty_file {
            std::fs::write(&path, []).unwrap();
        } else {
            std::fs::remove_file(&path).unwrap();
        }
        assert!(Journal::open(root.path(), fixture.trust).is_err());
        assert_eq!(
            path.exists(),
            keep_empty_file,
            "an anchored open cannot recreate history"
        );
        assert!(!root.path().join("control.checkpoint").exists());
    }
}

#[test]
fn malformed_uncommitted_suffixes_are_preserved_and_never_promoted() {
    use std::io::Write;
    let fixture = Fixture::new();
    for tail in [
        vec![0xff; 64],
        vec![0xa5, 0x71, 1, 0, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0],
    ] {
        let root = tmp();
        let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
        journal.ingest(fixture.nodes[0].bytes()).unwrap();
        drop(journal);
        let path = root.path().join("control.astream");
        let before = std::fs::read(&path).unwrap();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&tail).unwrap();
        aterm_durable::durable(&file).unwrap();
        drop(file);
        let journal = Journal::open(root.path(), fixture.trust).unwrap();
        assert_eq!(
            journal.state(),
            State::Ready {
                head: fixture.nodes[0].id(),
                basis: fixture.nodes[0].id()
            }
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let saved: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("control-uncommitted-")
            })
            .collect();
        assert_eq!(saved.len(), 1);
        assert_eq!(std::fs::read(&saved[0]).unwrap(), tail);
    }
}

#[test]
fn exhausted_suffix_archive_refuses_repair_without_discarding_bytes() {
    use std::io::Write;
    let fixture = Fixture::new();
    let root = tmp();
    let mut journal = Journal::open(root.path(), fixture.trust).unwrap();
    journal.ingest(fixture.nodes[0].bytes()).unwrap();
    drop(journal);
    private_file::write_private(root.path(), "control-uncommitted-filled", &[]).unwrap();
    OpenOptions::new()
        .write(true)
        .open(root.path().join("control-uncommitted-filled"))
        .unwrap()
        .set_len(MAX_BYTES as u64)
        .unwrap();
    let path = root.path().join("control.astream");
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&[0xff; 32]).unwrap();
    aterm_durable::durable(&file).unwrap();
    drop(file);
    let before = std::fs::read(&path).unwrap();
    assert!(Journal::open(root.path(), fixture.trust).is_err());
    assert_eq!(std::fs::read(path).unwrap(), before);
}

thread_local! {
    /// Nodes this thread authenticated through [`verify`].
    pub(super) static VERIFIED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
fn verified() -> usize {
    VERIFIED.with(std::cell::Cell::get)
}

#[test]
fn a_reader_succeeds_while_another_handle_holds_the_writer() {
    let fixture = Fixture::new();
    let dir = tmp();
    let mut owner = Journal::open(dir.path(), fixture.trust).unwrap();
    owner.ingest(fixture.nodes[0].bytes()).unwrap();
    owner.ingest(fixture.nodes[1].bytes()).unwrap();
    let busy = Journal::open_existing(dir.path(), fixture.trust)
        .err()
        .unwrap();
    assert_eq!(busy.kind(), crate::ErrorKind::JournalBusy);
    let mut reader = Journal::read_existing(dir.path(), fixture.trust).unwrap();
    assert!(owner.is_writer() && !reader.is_writer());
    assert!(reader.inventory().eq(owner.inventory()));
    assert_eq!(reader.state(), owner.state());
    for node in &fixture.nodes[..2] {
        assert_eq!(reader.receipt(&node.id()), owner.receipt(&node.id()));
    }
    assert!(reader.ingest(fixture.nodes[2].bytes()).is_err());
    owner.ingest(fixture.nodes[2].bytes()).unwrap();
    let reader = Journal::read_existing(dir.path(), fixture.trust).unwrap();
    assert!(reader.inventory().eq(owner.inventory()));
    assert_eq!(
        reader.receipt(&fixture.nodes[2].id()),
        owner.receipt(&fixture.nodes[2].id())
    );
    let mut foreign = fixture.trust;
    foreign.network[0] ^= 1;
    assert!(Journal::read_existing(dir.path(), foreign).is_err());
}

#[test]
fn a_kept_view_verifies_each_record_once_across_reads_and_writer_opens() {
    let fixture = Fixture::new();
    let dir = tmp();
    let mut writer = Journal::open(dir.path(), fixture.trust).unwrap();
    writer.ingest(fixture.nodes[0].bytes()).unwrap();
    writer.ingest(fixture.nodes[1].bytes()).unwrap();
    drop(writer);
    let start = verified();
    let first = Journal::read_existing(dir.path(), fixture.trust).unwrap();
    assert_eq!(
        verified() - start,
        2,
        "the first read verifies every record"
    );
    for _ in 0..3 {
        let again = Journal::read_existing(dir.path(), fixture.trust).unwrap();
        assert!(again.inventory().eq(first.inventory()));
    }
    let mut writer = Journal::open_existing(dir.path(), fixture.trust).unwrap();
    assert_eq!(writer.state(), first.state());
    assert_eq!(verified() - start, 2, "unchanged prefixes verify nothing");
    writer.ingest(fixture.nodes[2].bytes()).unwrap();
    drop(writer);
    assert_eq!(verified() - start, 3);
    let grown = Journal::read_existing(dir.path(), fixture.trust).unwrap();
    assert_eq!(grown.inventory().count(), 3);
    assert_eq!(
        verified() - start,
        4,
        "an extension verifies only its new record, never skipping it"
    );
    drop(Journal::open_existing(dir.path(), fixture.trust).unwrap());
    drop(Journal::read_existing(dir.path(), fixture.trust).unwrap());
    assert_eq!(verified() - start, 4);
}

#[test]
fn a_replaced_or_shortened_journal_rebuilds_its_view() {
    let fixture = Fixture::new();
    let dir = tmp();
    let mut writer = Journal::open(dir.path(), fixture.trust).unwrap();
    writer.ingest(fixture.nodes[0].bytes()).unwrap();
    let astream = dir.path().join("control.astream");
    let old = (
        std::fs::read(&astream).unwrap(),
        std::fs::read(dir.path().join("control.checkpoint")).unwrap(),
    );
    writer.ingest(fixture.nodes[1].bytes()).unwrap();
    drop(writer);
    assert_eq!(
        Journal::read_existing(dir.path(), fixture.trust)
            .unwrap()
            .inventory()
            .count(),
        2
    );

    // An older copy restored in place: the acknowledged prefix shrank.
    OpenOptions::new()
        .write(true)
        .open(&astream)
        .unwrap()
        .set_len(old.0.len() as u64)
        .unwrap();
    private_file::write_private(dir.path(), "control.checkpoint", &old.1).unwrap();
    let start = verified();
    let shorter = Journal::read_existing(dir.path(), fixture.trust).unwrap();
    assert_eq!(verified() - start, 1);
    assert!(
        shorter
            .inventory()
            .eq(std::iter::once(&fixture.nodes[0].id()))
    );

    // The same bytes under another inode are another file, verified afresh.
    let bytes = std::fs::read(&astream).unwrap();
    private_file::write_private(dir.path(), "control.astream", &bytes).unwrap();
    let replaced = Journal::read_existing(dir.path(), fixture.trust).unwrap();
    assert_eq!(verified() - start, 2);
    assert!(replaced.inventory().eq(shorter.inventory()));
}

#[test]
fn a_tampered_record_in_an_extension_is_refused_and_never_kept() {
    let fixture = Fixture::new();
    let dir = tmp();
    let mut writer = Journal::open(dir.path(), fixture.trust).unwrap();
    writer.ingest(fixture.nodes[0].bytes()).unwrap();
    drop(writer);
    drop(Journal::read_existing(dir.path(), fixture.trust).unwrap());
    attest_tampered(&fixture, dir.path());
    let start = verified();
    assert!(Journal::read_existing(dir.path(), fixture.trust).is_err());
    assert_eq!(
        verified() - start,
        1,
        "only the extension was authenticated"
    );
    assert!(
        Journal::read_existing(dir.path(), fixture.trust).is_err(),
        "the refused record never entered the kept view"
    );
    assert_eq!(
        verified() - start,
        2,
        "the kept view still ends before the refused record"
    );
    assert!(Journal::open_existing(dir.path(), fixture.trust).is_err());
}

/// Whoever can write the directory can acknowledge any bytes: append a record
/// whose node no longer authenticates, under a valid checkpoint.
fn attest_tampered(fixture: &Fixture, dir: &Path) {
    let mut tampered = fixture.nodes[1].bytes().to_vec();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    assert!(verify(fixture.trust, &tampered).is_err());
    let mut raw = Log::open(dir, NAME, &fixture.trust.bytes(), true, LIMITS).unwrap();
    raw.finish_open().unwrap();
    let mut body = 7u64.to_le_bytes().to_vec();
    body.extend_from_slice(&tampered);
    raw.append(SUBJECT, body).unwrap();
}

#[test]
fn a_tampered_record_read_with_no_kept_view_is_refused_by_reader_and_writer() {
    let fixture = Fixture::new();
    let dir = tmp();
    let mut writer = Journal::open(dir.path(), fixture.trust).unwrap();
    writer.ingest(fixture.nodes[0].bytes()).unwrap();
    drop(writer);
    attest_tampered(&fixture, dir.path());
    let start = verified();
    assert!(Journal::read_existing(dir.path(), fixture.trust).is_err());
    assert_eq!(
        verified() - start,
        2,
        "a cold read replays from offset zero"
    );
    assert!(Journal::open_existing(dir.path(), fixture.trust).is_err());
    assert!(Journal::read_existing(dir.path(), fixture.trust).is_err());
}

#[test]
fn the_view_cache_keeps_the_most_recently_read_journals() {
    let fixture = Fixture::new();
    let dirs = [tmp(), tmp(), tmp()];
    for dir in &dirs {
        let mut writer = Journal::open(dir.path(), fixture.trust).unwrap();
        writer.ingest(fixture.nodes[0].bytes()).unwrap();
    }
    let views = Views::new(2);
    let read = |at: usize| {
        let start = verified();
        let (view, _) = views.read(dirs[at].path(), fixture.trust).unwrap();
        assert!(view.nodes.contains_key(&fixture.nodes[0].id()));
        verified() - start
    };
    assert_eq!((read(0), read(1)), (1, 1), "a first read verifies");
    assert_eq!(read(0), 0, "each directory keeps its own view");
    assert_eq!(read(2), 1);
    assert_eq!(read(0), 0, "reading a view keeps it");
    assert_eq!(read(1), 1, "the least recently read view went first");
    assert_eq!(read(2), 1);
}

thread_local! {
    /// Pause one read after it acquired its cache entry.
    static BEFORE_VIEW: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    /// Run once, when a writer open has read its view and not yet taken the lock.
    static BEFORE_LOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}
pub(super) fn before_view() {
    let hook = BEFORE_VIEW.with(|hook| hook.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[test]
fn journal_reads_do_not_wait_for_unrelated_views() {
    use std::{sync::mpsc, thread, time::Duration};

    let fixture = Fixture::new();
    let dirs = [tmp(), tmp()];
    for dir in &dirs {
        let mut writer = Journal::open(dir.path(), fixture.trust).unwrap();
        writer.ingest(fixture.nodes[0].bytes()).unwrap();
    }
    let views = Views::new(2);
    let trust = fixture.trust;
    thread::scope(|scope| {
        let views = &views;
        let first = dirs[0].path();
        let second = dirs[1].path();
        let (started, observed) = mpsc::channel();
        let (release, blocked) = mpsc::channel::<()>();
        let first_read = scope.spawn(move || {
            BEFORE_VIEW.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    started.send(()).unwrap();
                    let _ = blocked.recv();
                }));
            });
            views.read(first, trust)
        });
        observed.recv_timeout(Duration::from_secs(60)).unwrap();
        let (completed, completion) = mpsc::channel();
        let second_read = scope.spawn(move || {
            completed
                .send(views.read(second, trust).map(|(view, _)| view.nodes.len()))
                .unwrap();
        });
        let result = completion.recv_timeout(Duration::from_secs(60));
        // Release the paused read before asserting, including the negative
        // control where a global cache lock prevents the second read.
        drop(release);
        assert_eq!(first_read.join().unwrap().unwrap().0.nodes.len(), 1);
        second_read.join().unwrap();
        assert_eq!(
            result
                .expect("another journal read could not finish")
                .unwrap(),
            1
        );
    });
}

pub(super) fn before_lock() {
    let hook = BEFORE_LOCK.with(|hook| hook.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[test]
fn a_writer_never_keeps_a_view_another_writer_extended_before_its_lock() {
    let fixture = Fixture::new();
    let dir = tmp();
    let mut writer = Journal::open(dir.path(), fixture.trust).unwrap();
    writer.ingest(fixture.nodes[0].bytes()).unwrap();
    drop(writer);
    drop(Journal::read_existing(dir.path(), fixture.trust).unwrap());
    // Another writer lands a record after this open read its view and before
    // it takes the lock.
    let landed = std::rc::Rc::new(std::cell::Cell::new(None));
    let receipt = std::rc::Rc::clone(&landed);
    let (path, trust, wire) = (
        dir.path().to_path_buf(),
        fixture.trust,
        fixture.nodes[1].bytes().to_vec(),
    );
    BEFORE_LOCK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut other = Journal::open_existing(&path, trust).unwrap();
            receipt.set(Some(other.ingest(&wire).unwrap()));
        }));
    });
    let mut outer = Journal::open_existing(dir.path(), fixture.trust).unwrap();
    let landed = landed.get().expect("the other writer appended first");
    assert_eq!(outer.receipt(&fixture.nodes[1].id()), Some(landed));
    assert_eq!(outer.inventory().count(), 2);
    let bytes = std::fs::metadata(dir.path().join("control.astream"))
        .unwrap()
        .len();
    assert_eq!(outer.ingest(fixture.nodes[1].bytes()).unwrap(), landed);
    assert_eq!(
        std::fs::metadata(dir.path().join("control.astream"))
            .unwrap()
            .len(),
        bytes,
        "a record the other writer landed is never appended twice"
    );
}

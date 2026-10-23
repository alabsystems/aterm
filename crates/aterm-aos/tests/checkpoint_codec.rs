// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Checkpoint frame, state digest, and identity/canonical-state restore checks.

use astream_wire::frame::{Frame, decode_frame_slice};
use aterm_aos::checkpoint::{
    CHECKPOINT_MAGIC, CHECKPOINT_VERSION, Checkpoint, CheckpointFault, Provenance, RestoreRefusal,
};
use aterm_aos::log::IdentityMismatch;
use aterm_aos::task::{STATE_DOMAIN, Schema, state_digest};
use aterm_aos::wire::{Enc, WireFault};
use aterm_aos::{ArtifactId, ArtifactKind, BranchId, Digest, Revision, TaskIdentity};

use aterm_aos::publish::PublishRefusal;

use crate::toy::{ToySchema, ToyState, Variant, attached, identity};

fn provenance(id: TaskIdentity) -> Provenance {
    Provenance {
        identity: id,
        log_epoch: 2,
        input_cursor: 17,
        effect_cursor: 3,
        branch: BranchId(1),
        revision: Revision(4),
        parent: Some(Digest([9; 32])),
    }
}

fn state() -> ToyState {
    ToyState {
        total: -42,
        hot: true,
        seen: 77,
    }
}

fn sample() -> (Checkpoint, Vec<u8>) {
    let c = Checkpoint::capture(
        &ToySchema::new(),
        &state(),
        provenance(identity(Variant::Good)),
    );
    let bytes = c.encode().unwrap();
    (c, bytes)
}

fn body_of(bytes: &[u8]) -> Vec<u8> {
    decode_frame_slice(bytes).unwrap().unwrap().0.to_vec()
}

fn reframe(body: Vec<u8>) -> Vec<u8> {
    Frame::new(body).encode().unwrap()
}

fn body(p: &Provenance, state_bytes: &[u8]) -> Vec<u8> {
    let mut e = Enc::new();
    e.raw(&CHECKPOINT_MAGIC);
    e.u8(CHECKPOINT_VERSION);
    e.identity(&p.identity);
    e.u64(p.log_epoch);
    e.u64(p.input_cursor);
    e.u64(p.effect_cursor);
    e.u64(p.branch.0);
    e.u64(p.revision.0);
    e.opt(p.parent.as_ref(), |e, d| e.digest(d));
    e.digest(&Digest::of(
        STATE_DOMAIN,
        &[&p.identity.schema.digest.0, state_bytes],
    ));
    e.bytes(state_bytes);
    e.finish()
}

#[test]
fn a_checkpoint_round_trips_with_its_digests_and_state() {
    let schema = ToySchema::new();
    let (c, bytes) = sample();
    let back = Checkpoint::decode(&bytes).unwrap();
    assert_eq!(back, c);
    assert_eq!(back.digest(), c.digest());
    assert_eq!(back.state_digest(), state_digest(&schema, &state()));
    assert_eq!(back.provenance(), &provenance(identity(Variant::Good)));
    assert_eq!(back.restore(&schema, &identity(Variant::Good)), Ok(state()));
    let mut by_hand = Enc::new();
    schema.encode_state(&state(), &mut by_hand);
    assert_eq!(body_of(&bytes), body(c.provenance(), by_hand.as_bytes()));
}

#[test]
fn the_checkpoint_digest_covers_its_provenance_and_state() {
    let schema = ToySchema::new();
    let (c, _) = sample();
    let moved = Checkpoint::capture(
        &schema,
        &state(),
        Provenance {
            input_cursor: 18,
            ..provenance(identity(Variant::Good))
        },
    );
    assert_ne!(moved.digest(), c.digest());
    assert_eq!(moved.state_digest(), c.state_digest());
    let other_state = Checkpoint::capture(
        &schema,
        &ToyState {
            seen: 78,
            ..state()
        },
        provenance(identity(Variant::Good)),
    );
    assert_ne!(other_state.digest(), c.digest());
}

#[test]
fn every_byte_flip_of_a_checkpoint_is_refused_as_a_frame_fault() {
    let (_, bytes) = sample();
    for at in 0..bytes.len() {
        for mask in [0x01u8, 0x80, 0xff] {
            let mut bad = bytes.clone();
            bad[at] ^= mask;
            let got = Checkpoint::decode(&bad);
            assert!(
                matches!(got, Err(CheckpointFault::Frame(_))),
                "byte {at} mask {mask:#x}: {got:?}"
            );
        }
    }
}

#[test]
fn every_truncation_of_a_checkpoint_is_refused_as_a_frame_fault() {
    let (_, bytes) = sample();
    for cut in 0..bytes.len() {
        let got = Checkpoint::decode(&bytes[..cut]);
        assert!(
            matches!(got, Err(CheckpointFault::Frame(_))),
            "cut {cut}: {got:?}"
        );
    }
}

#[test]
fn bytes_after_the_frame_are_refused() {
    let (_, mut bytes) = sample();
    bytes.push(0);
    assert_eq!(
        Checkpoint::decode(&bytes),
        Err(CheckpointFault::Wire(WireFault::Trailing { extra: 1 }))
    );
}

#[test]
fn a_checkpoint_of_another_version_is_refused_by_its_number() {
    let (_, bytes) = sample();
    let mut b = body_of(&bytes);
    b[CHECKPOINT_MAGIC.len()] = 2;
    assert_eq!(
        Checkpoint::decode(&reframe(b)),
        Err(CheckpointFault::Version(2))
    );
}

#[test]
fn a_frame_that_holds_no_checkpoint_is_refused_as_not_a_checkpoint() {
    assert_eq!(
        Checkpoint::decode(&reframe(b"AOSLOG\0\x01 and more".to_vec())),
        Err(CheckpointFault::NotACheckpoint)
    );
    assert_eq!(
        Checkpoint::decode(&reframe(b"short".to_vec())),
        Err(CheckpointFault::NotACheckpoint)
    );
}

#[test]
fn a_whole_frame_whose_state_bytes_disagree_with_the_stored_digest_is_refused() {
    let (_, bytes) = sample();
    let mut b = body_of(&bytes);
    let last = b.len() - 1;
    b[last] ^= 0x01;
    assert_eq!(
        Checkpoint::decode(&reframe(b)),
        Err(CheckpointFault::StateDigest)
    );
}

#[test]
fn a_body_with_bytes_after_the_state_is_refused() {
    let (_, bytes) = sample();
    let mut b = body_of(&bytes);
    b.push(0);
    assert_eq!(
        Checkpoint::decode(&reframe(b)),
        Err(CheckpointFault::Wire(WireFault::Trailing { extra: 1 }))
    );
}

#[test]
fn restore_refuses_another_task_schema_or_profile_and_accepts_other_code() {
    let schema = ToySchema::new();
    let (c, _) = sample();
    let good = identity(Variant::Good);
    let other = |a: &ArtifactId| ArtifactId {
        version: a.version + 1,
        ..a.clone()
    };
    for (expect, which) in [
        (
            TaskIdentity {
                task: other(&good.task),
                ..good.clone()
            },
            IdentityMismatch::Task,
        ),
        (
            TaskIdentity {
                schema: other(&good.schema),
                ..good.clone()
            },
            IdentityMismatch::Schema,
        ),
        (
            TaskIdentity {
                profile: other(&good.profile),
                ..good.clone()
            },
            IdentityMismatch::Profile,
        ),
    ] {
        assert_eq!(
            c.restore(&schema, &expect),
            Err(RestoreRefusal::Identity(which))
        );
    }
    assert_eq!(
        c.restore(&schema, &identity(Variant::Stranded)),
        Ok(state())
    );
}

#[test]
fn a_checkpoint_whose_provenance_names_another_schema_is_refused_on_decode_and_restore() {
    let schema = ToySchema::new();
    let wrong = TaskIdentity {
        schema: ArtifactId::new(ArtifactKind::Schema, "toy-schema", 2, b"total,hot,seen"),
        ..identity(Variant::Good)
    };
    let c = Checkpoint::capture(&schema, &state(), provenance(wrong.clone()));
    assert_eq!(
        Checkpoint::decode(&c.encode().unwrap()),
        Err(CheckpointFault::StateDigest)
    );
    assert_eq!(
        c.restore(&schema, &wrong),
        Err(RestoreRefusal::Identity(IdentityMismatch::Schema))
    );
}

#[test]
fn restore_refuses_a_checkpoint_whose_state_digest_is_not_under_the_restoring_schema() {
    let named = identity(Variant::Good);
    let captured_under = ToySchema::declared(ArtifactId::new(
        ArtifactKind::Schema,
        "toy-schema",
        2,
        b"total,hot,seen",
    ));
    let c = Checkpoint::capture(&captured_under, &state(), provenance(named.clone()));
    let schema = ToySchema::new();
    let stored = state_digest(&captured_under, &state());
    let derived = state_digest(&schema, &state());
    assert_ne!(stored, derived);
    assert_eq!(c.state_digest(), stored);
    assert_eq!(
        c.restore(&schema, &named),
        Err(RestoreRefusal::StateDigest { stored, derived })
    );
    let honest = Checkpoint::capture(&schema, &state(), provenance(named.clone()));
    assert_eq!(honest.restore(&schema, &named), Ok(state()));

    let mut b = attached(Variant::Good, 4);
    let head = b.head().clone();
    assert_eq!(
        b.restore(&c),
        Err(PublishRefusal::Restore(RestoreRefusal::StateDigest {
            stored,
            derived
        }))
    );
    assert_eq!(b.head(), &head);
}

#[test]
fn restore_refuses_a_state_the_schema_does_not_decode() {
    let c = Checkpoint::capture(
        &ToySchema::lossy(),
        &state(),
        provenance(identity(Variant::Good)),
    );
    assert_eq!(
        c.restore(&ToySchema::new(), &identity(Variant::Good)),
        Err(RestoreRefusal::Wire(WireFault::BadTag {
            field: "toy.hot",
            tag: 77
        }))
    );
}

#[test]
fn restore_refuses_a_state_its_schema_decodes_but_would_not_write() {
    let p = provenance(identity(Variant::Good));
    let mut state_bytes = Enc::new();
    state_bytes.i64(5);
    state_bytes.u8(2);
    state_bytes.u64(0);
    let bytes = reframe(body(&p, state_bytes.as_bytes()));
    let c = Checkpoint::decode(&bytes).unwrap();
    assert_eq!(
        c.restore(&ToySchema::sloppy(), &identity(Variant::Good)),
        Err(RestoreRefusal::Wire(WireFault::NonCanonical {
            field: "state"
        }))
    );
    assert_eq!(
        c.restore(&ToySchema::new(), &identity(Variant::Good)),
        Err(RestoreRefusal::Wire(WireFault::BadTag {
            field: "toy.hot",
            tag: 2
        }))
    );
}

#[test]
fn restoring_leaves_the_checkpoint_and_its_bytes_unchanged() {
    let schema = ToySchema::new();
    let (c, bytes) = sample();
    let before = c.clone();
    for _ in 0..3 {
        assert_eq!(c.restore(&schema, &identity(Variant::Good)), Ok(state()));
    }
    assert_eq!(c, before);
    assert_eq!(c.encode().unwrap(), bytes);
    assert_eq!(
        Checkpoint::decode(&bytes).unwrap().digest(),
        before.digest()
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The frame limit is checked against the complete wire body before any output allocates.

use super::*;
use crate::ident::{ArtifactId, ArtifactKind};
use astream_wire::frame::{Frame, decode_frame_slice};

fn checkpoint(state_len: usize, parent: bool) -> Checkpoint {
    let identity = TaskIdentity {
        task: ArtifactId::new(ArtifactKind::Task, "task", 1, b"task"),
        code: ArtifactId::new(ArtifactKind::Code, "code", 2, b"code"),
        contract: ArtifactId::new(ArtifactKind::Contract, "contract", 3, b"contract"),
        schema: ArtifactId::new(ArtifactKind::Schema, "schema", 4, b"schema"),
        profile: ArtifactId::new(ArtifactKind::Profile, "profile", 5, b"profile"),
    };
    let state = vec![0x5a; state_len];
    let state_digest = state_digest_of(&identity.schema.digest, &state);
    Checkpoint {
        provenance: Provenance {
            identity,
            log_epoch: 1,
            input_cursor: 2,
            effect_cursor: 3,
            branch: BranchId(4),
            revision: Revision(5),
            parent: parent.then_some(Digest([0x23; 32])),
        },
        state,
        state_digest,
    }
}

fn artifact(c: &mut Checkpoint, index: usize) -> &mut ArtifactId {
    let id = &mut c.provenance.identity;
    match index {
        0 => &mut id.task,
        1 => &mut id.code,
        2 => &mut id.contract,
        3 => &mut id.schema,
        4 => &mut id.profile,
        _ => panic!("fixture has five artifacts"),
    }
}

/// Canonical wire output through the existing field encoder, independent of size accounting.
fn canonical_body(c: &Checkpoint) -> Vec<u8> {
    let mut out = Enc::new();
    out.raw(&CHECKPOINT_MAGIC);
    out.u8(CHECKPOINT_VERSION);
    encode_provenance(&mut out, &c.provenance);
    out.digest(&c.state_digest);
    out.bytes(&c.state);
    out.finish()
}

#[test]
fn checkpoint_preflight_preserves_wire_bytes_for_every_artifact_and_parent() {
    for parent in [false, true] {
        for index in 0..5 {
            for state_len in [0, 1, 257, 4096] {
                let mut c = checkpoint(state_len, parent);
                artifact(&mut c, index).name = "名é🦀".repeat(index + 1);
                let body = canonical_body(&c);
                assert_eq!(c.encoded_body_len(), Some(body.len()));
                let expected = Frame::new(body).encode().unwrap();
                assert_eq!(c.encode().unwrap(), expected);
                assert_eq!(Checkpoint::decode(&expected).unwrap(), c);
            }
        }
    }
    let without = checkpoint(0, false).encoded_body_len().unwrap();
    let with = checkpoint(0, true).encoded_body_len().unwrap();
    assert_eq!(with - without, 32);
}

#[test]
fn a_checkpoint_at_the_frame_cap_succeeds_and_one_byte_more_allocates_nothing() {
    for parent in [false, true] {
        let overhead = canonical_body(&checkpoint(0, parent)).len();
        let mut c = checkpoint(MAX_PAYLOAD_LEN - overhead, parent);
        let (result, allocations) = aterm_heap_count::allocations_in(|| c.encode());
        let encoded = result.unwrap();
        assert!(
            allocations > 0,
            "the successful frame exercises the heap counter"
        );
        assert_eq!(
            decode_frame_slice(&encoded).unwrap().unwrap().0.len(),
            MAX_PAYLOAD_LEN
        );
        assert_eq!(encoded.len(), HEADER_SIZE + MAX_PAYLOAD_LEN);
        assert_eq!(Checkpoint::decode(&encoded).unwrap(), c);
        drop(encoded);

        c.state.push(0x5a);
        c.state_digest = state_digest_of(&c.provenance.identity.schema.digest, &c.state);
        let before = c.clone();
        let (result, allocations) = aterm_heap_count::allocations_in(|| c.encode());
        assert_eq!(
            result,
            Err(CheckpointFault::TooLarge {
                len: MAX_PAYLOAD_LEN + 1,
                limit: MAX_PAYLOAD_LEN,
            })
        );
        assert_eq!(
            allocations, 0,
            "refusal precedes the frame allocation and state copy"
        );
        assert_eq!(c, before);
    }
}

#[test]
fn oversized_provenance_in_each_artifact_is_refused_without_allocating() {
    for parent in [false, true] {
        for index in 0..5 {
            let mut c = checkpoint(0, parent);
            let old_name_len = artifact(&mut c, index).name.len();
            let overhead = canonical_body(&c).len() - old_name_len;
            artifact(&mut c, index).name = "p".repeat(MAX_PAYLOAD_LEN + 1 - overhead);
            let before = c.clone();
            let (result, allocations) = aterm_heap_count::allocations_in(|| c.encode());
            assert_eq!(
                result,
                Err(CheckpointFault::TooLarge {
                    len: MAX_PAYLOAD_LEN + 1,
                    limit: MAX_PAYLOAD_LEN,
                })
            );
            assert_eq!(
                allocations, 0,
                "provenance must be measured before it is encoded"
            );
            assert_eq!(c, before);
        }
    }
}

#[test]
fn wire_length_prefix_accounting_checks_its_boundary_and_arithmetic() {
    assert_eq!(encoded_bytes_len(0), Some(4));
    let boundary = usize::try_from(u32::MAX).unwrap();
    assert_eq!(
        encoded_bytes_len(boundary - 1),
        (boundary - 1).checked_add(4)
    );
    assert_eq!(encoded_bytes_len(boundary), boundary.checked_add(12));
    assert_eq!(encoded_bytes_len(usize::MAX), None);
}

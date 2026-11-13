// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn empty_counts_do_not_hide_unresolved_or_release_custody() {
    let mut sample = SourceProofObservation {
        issued: 0,
        member_highwater: 0,
        member_epoch: String::new(),
        members: 0,
        managers_ready: 0,
        readers_ready: 0,
        loaded_readers: Vec::new(),
        unresolved: 0,
        unsettled_custody: 0,
        active_uses: 0,
        pending_uses: 0,
        ending_uses: 0,
        maximum_credential_generation: 0,
        ready_members: Vec::new(),
        uses: Vec::new(),
        derived: Vec::new(),
    };
    assert!(sample.closed());
    sample.unresolved = 1;
    assert!(!sample.closed());
    sample.unresolved = 0;
    sample.unsettled_custody = 1;
    assert!(!sample.closed());
}
#[test]
fn maximum_registry_metadata_and_receipts_fit_native_bound() {
    let word = "f".repeat(32);
    let sample = SourceProofObservation {
        issued: 128,
        member_highwater: u64::MAX,
        member_epoch: "f".repeat(16),
        members: 128,
        managers_ready: 128,
        readers_ready: 128,
        loaded_readers: vec![word.clone(); 128],
        unresolved: 128,
        unsettled_custody: 4096,
        active_uses: 512,
        pending_uses: 512,
        ending_uses: 512,
        maximum_credential_generation: u64::MAX,
        ready_members: (0..128)
            .map(|_| SourceProofMember {
                member: word.clone(),
                operation: word.clone(),
                preload_sequence: word.clone(),
                connection: u64::MAX,
                registration: word.clone(),
                credential_generation: u64::MAX,
                report: [u64::MAX; 4],
            })
            .collect(),
        uses: (0..512)
            .map(|_| SourceProofUse {
                member: word.clone(),
                nonce: word.clone(),
                manager: true,
                reader: true,
            })
            .collect(),
        derived: (0..128)
            .map(|_| SourceProofDerived {
                child: word.clone(),
                parent: word.clone(),
                nonce: word.clone(),
            })
            .collect(),
    };
    let event = aterm_json::json!({"action":"member-rebound-ready","request":word,"operation":word,
        "member":word,"registration":word,"preload_sequence":word,"credential_generation":u64::MAX,
        "report":vec![u64::MAX;4],"connection":u64::MAX,"sequence":u64::MAX});
    let encoded = aterm_json::to_vec(
        &aterm_json::json!({"schema":1,"request":word,"action":"observe",
        "observation":sample,"outcomes":vec![event;32],"outcomeSequence":u64::MAX,
        "readySeen":true,"closedSeen":false,"generation":u64::MAX,"outcome":"ok"}),
    )
    .unwrap();
    assert!(encoded.len() > 4096);
    assert!(encoded.len() <= SOURCE_PROOF_OBSERVATION_LIMIT);
}

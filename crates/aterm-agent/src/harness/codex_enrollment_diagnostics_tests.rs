// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn request() -> Request {
    Request {
        id: "a".repeat(32),
        caller: "b".repeat(64),
        label: "main".into(),
        revision: "c".repeat(64),
    }
}

fn admitted(request: &Request) -> Operations {
    let mut book = Operations::default();
    let before = book.clone();
    book.admit_native(request).unwrap();
    book.commit(&before, &Default::default(), &Default::default())
        .unwrap();
    book
}

#[test]
fn typed_preparation_refusal_roundtrips_without_granting_recovery() {
    let request = request();
    for stage in [
        NativePreparationStage::RowPolicy,
        NativePreparationStage::GrantCustody,
        NativePreparationStage::NativeScope,
        NativePreparationStage::RowAdmission,
    ] {
        let mut book = admitted(&request);
        let original = book.encode();
        assert!(
            original[request.id.as_str()]
                .get("preparation_stage")
                .is_none()
        );
        assert_eq!(Operations::decode(&original).unwrap(), book);
        let before = book.clone();
        book.refuse_preparation(&request, stage).unwrap();
        book.commit(&before, &Default::default(), &Default::default())
            .unwrap();
        let encoded = book.encode();
        assert_eq!(
            encoded[request.id.as_str()]["preparation_stage"].as_str(),
            Some(stage.as_str())
        );
        let restored = Operations::decode(&encoded).unwrap();
        let (intent, status, actual) = restored.diagnostics(&request.id, &request.caller).unwrap();
        assert_eq!(intent, request.intent());
        assert_eq!(status.phase, Phase::Refused);
        assert_eq!(status.generation, None);
        assert_eq!(actual, Some(stage));
        assert!(restored.require_running(&request).is_err());
        assert!(
            restored
                .require_native_recovery_candidate(&request)
                .is_err()
        );
        assert_eq!(
            restored.encode(),
            encoded,
            "diagnostics never writes custody"
        );
    }
}

#[test]
fn diagnostic_accessor_rejects_unknown_and_cross_caller_operations_identically() {
    let request = request();
    let book = admitted(&request);
    let unknown = book.diagnostics(&"d".repeat(32), &request.caller);
    let foreign = book.diagnostics(&request.id, &"e".repeat(64));
    assert!(unknown.is_err());
    assert_eq!(unknown, foreign);
    assert!(book.diagnostics(&request.id, "invalid").is_err());
    assert!(book.diagnostics("invalid", &request.caller).is_err());
    assert_eq!(
        book.diagnostics(&request.id, &request.caller).unwrap().2,
        None
    );
}

#[test]
fn preparation_metadata_needs_its_transaction_marker_and_is_immutable() {
    let request = request();
    let before = admitted(&request);
    let mut forged = before.clone();
    forged.finish(&request, Phase::Refused).unwrap();
    forged.0.get_mut(&request.id).unwrap().preparation_stage =
        Some(NativePreparationStage::NativeScope);
    assert!(
        forged
            .commit(&before, &Default::default(), &Default::default())
            .is_err()
    );
    let mut book = before.clone();
    book.refuse_preparation(&request, NativePreparationStage::RowPolicy)
        .unwrap();
    book.commit(&before, &Default::default(), &Default::default())
        .unwrap();
    for next in [None, Some(NativePreparationStage::GrantCustody)] {
        let mut changed = book.clone();
        changed.0.get_mut(&request.id).unwrap().preparation_stage = next;
        assert!(
            changed
                .commit(&book, &Default::default(), &Default::default())
                .is_err()
        );
    }
    for phase in [
        Phase::Running,
        Phase::AuthenticationRequired,
        Phase::Applied,
    ] {
        let mut invalid = book.clone();
        invalid.0.get_mut(&request.id).unwrap().status.phase = phase;
        assert!(Operations::decode(&invalid.encode()).is_err());
    }
    let mut invalid = book.encode();
    invalid
        .as_object_mut()
        .unwrap()
        .get_mut(request.id.as_str())
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert(
            "preparation_stage".into(),
            aterm_json::json!("private nested diagnostic /private/untrusted"),
        );
    assert!(Operations::decode(&invalid).is_err());
}

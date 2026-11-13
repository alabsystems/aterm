// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::json;

fn draft(expected: Option<Digest>) -> Draft {
    Draft::new(
        Destination::new([1; 16], [2; 32], [3; 32], [4; 16], 1, [5; 48]).unwrap(),
        "accounts".into(),
        "one".into(),
        expected,
        json!({"provider":"claude", "method":"cli", "email":"one@example.test"}),
    )
    .unwrap()
}

#[test]
fn retained_identity_binds_condition_destination_and_command() {
    let pending = draft(None).allocate_with([6; 16]).unwrap();
    assert_eq!(
        Pending::decode(&pending.encode().unwrap()).unwrap(),
        pending
    );
    for field in ["condition", "destination", "command", "nonce", "unknown"] {
        let value = aterm_json::to_value(&pending).unwrap();
        let mut fields = value.as_object().unwrap().clone();
        match field {
            "condition" => {
                fields.insert(
                    "condition".into(),
                    json!({"kind":"existing", "operation":hex::encode_lower(&[7; 32])}),
                );
            }
            "destination" => {
                fields
                    .get_mut("destination")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert("actor".into(), Value::String(hex::encode_lower(&[7; 48])));
            }
            "command" => {
                let mut command = pending.validate().unwrap();
                let Command::Write { changes, .. } = &mut command else {
                    unreachable!()
                };
                changes[0]
                    .value
                    .as_mut()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert("email".into(), Value::String("two@example.test".into()));
                fields.insert(
                    "command".into(),
                    Value::String(String::from_utf8(command.encode().unwrap()).unwrap()),
                );
            }
            "nonce" => {
                fields.insert("nonce".into(), Value::String(hex::encode_lower(&[7; 16])));
            }
            _ => {
                fields.insert("unrecognized".into(), Value::Bool(true));
            }
        }
        assert!(
            Pending::decode(&aterm_json::to_vec(&fields).unwrap()).is_err(),
            "{field}"
        );
    }
    let again = draft(None).allocate_with([7; 16]).unwrap();
    let changed_condition = draft(Some([7; 32])).allocate_with([6; 16]).unwrap();
    assert_ne!(
        pending.validate().unwrap().request(),
        again.validate().unwrap().request()
    );
    assert_ne!(
        pending.validate().unwrap().request(),
        changed_condition.validate().unwrap().request()
    );
}

#[test]
fn original_v1_bindings_survive_decode_and_reencode() {
    // Frozen request IDs from the v1 canonical binding, independently hashed.
    for (expected, request) in [
        (None, "6f26e91421ae7d6c724c07cfe8881f05"),
        (Some([7; 32]), "710dd53850bbfcb85b59c219ba859a3b"),
    ] {
        let draft = draft(expected);
        let request: [u8; 16] = decode_hex(request).unwrap();
        let wire = LegacyWire {
            schema: 1,
            destination: draft.destination,
            expected: expected.map(|id| hex::encode_lower(&id)),
            nonce: hex::encode_lower(&[6; 16]),
            command: bounded_command(request, &[draft.change]).unwrap(),
        };
        let bytes = aterm_json::to_vec(&wire).unwrap();
        let pending = Pending::decode(&bytes).unwrap();
        assert_eq!(pending.request().unwrap(), request);
        assert_eq!(pending.encode().unwrap(), bytes);
        let next = Draft::after(&pending, json!({"provider":"claude", "method":"cli"}))
            .unwrap()
            .allocate_with([8; 16])
            .unwrap();
        assert_eq!(next.schema, 2);
        assert!(matches!(next.condition, Condition::After { .. }));
        assert_eq!(Pending::decode(&next.encode().unwrap()).unwrap(), next);
    }
}

#[test]
fn predecessor_binding_and_version_shapes_cannot_be_changed() {
    let predecessor = draft(None).allocate_with([6; 16]).unwrap();
    let pending = Draft::after(&predecessor, json!({"provider":"claude", "method":"cli"}))
        .unwrap()
        .allocate_with([8; 16])
        .unwrap();
    for field in ["request", "command_sha256", "schema", "legacy", "unknown"] {
        let mut wire = aterm_json::to_value(&pending).unwrap();
        let fields = wire.as_object_mut().unwrap();
        match field {
            "request" | "command_sha256" => {
                fields
                    .get_mut("condition")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert(
                        field.into(),
                        json!("09".repeat(if field == "request" { 16 } else { 32 })),
                    );
            }
            "schema" => {
                fields.insert("schema".into(), json!(1));
            }
            "legacy" => {
                fields.insert("expected".into(), Value::Null);
            }
            _ => {
                fields
                    .get_mut("condition")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert("extra".into(), Value::Null);
            }
        }
        assert!(
            Pending::decode(&aterm_json::to_vec(&wire).unwrap()).is_err(),
            "{field}"
        );
    }
}

#[test]
fn only_bounded_classified_metadata_can_be_queued() {
    let destination = draft(None).destination;
    for (collection, key, value) in [
        (
            "budgets",
            "one",
            json!({"provider":"claude", "method":"cli"}),
        ),
        (
            "accounts",
            "../one",
            json!({"provider":"claude", "method":"cli"}),
        ),
        (
            "accounts",
            "one",
            json!({"provider":"claude", "method":"cli", "unclassified":"value"}),
        ),
        ("phones", "one", json!({"number":"not-a-number"})),
        ("phones", "one", Value::Null),
    ] {
        assert!(
            Draft::new(
                destination.clone(),
                collection.into(),
                key.into(),
                None,
                value
            )
            .is_err()
        );
    }
    let mut value = json!({"provider":"claude", "method":"cli", "details":{}});
    for n in 0..40 {
        value
            .as_object_mut()
            .unwrap()
            .get_mut("details")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(
                format!("note{n}"),
                json!({"kind":"note", "value":"x".repeat(2048)}),
            );
    }
    assert!(Draft::new(destination, "accounts".into(), "one".into(), None, value).is_err());
}

#[test]
fn destination_rejects_zero_custody_components() {
    for field in 0..6 {
        assert!(
            Destination::new(
                if field == 0 { [0; 16] } else { [1; 16] },
                if field == 1 { [0; 32] } else { [2; 32] },
                if field == 2 { [0; 32] } else { [3; 32] },
                if field == 3 { [0; 16] } else { [4; 16] },
                if field == 4 { 0 } else { 1 },
                if field == 5 { [0; 48] } else { [5; 48] },
            )
            .is_err(),
            "component {field}"
        );
    }
}

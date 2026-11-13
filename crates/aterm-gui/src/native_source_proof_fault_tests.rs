// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn historical_ack_on_cleanup_connection_cannot_fire_current_work_fault() {
    // Unattested metadata exercises refusal, not native Ready admission.
    let event = aterm_json::json!({"member":"a".repeat(32),"operation":"b".repeat(32),
        "preload_sequence":"c".repeat(32),"registration":"d".repeat(32),"connection":7,
        "credential_generation":4,"report":[4,1,2,1]});
    assert!(current_projection(&event, &event));
    for field in [
        "connection",
        "operation",
        "preload_sequence",
        "registration",
        "credential_generation",
        "report",
    ] {
        let mut current = event.clone();
        current
            .as_object_mut()
            .unwrap()
            .insert(field.into(), aterm_json::json!("foreign"));
        assert!(
            !current_projection(&event, &current),
            "historical {field} does not identify the current work occurrence"
        );
    }
    assert!(!current_projection(
        &aterm_json::json!({}),
        &aterm_json::json!({})
    ));
}
#[test]
fn loss_is_one_shot_and_only_matches_the_new_native_manager_report() {
    // Correlation metadata only, never a manufactured native Ready grant.
    let mut loss = ReadyLoss {
        member: "a".repeat(32),
        credential_generation: 4,
        previous_registration: "b".repeat(32),
        fired: None,
    };
    let accepted = aterm_json::json!({"action":"member-ready","member":"a".repeat(32),
        "credential_generation":4,"registration":"c".repeat(32),"connection":7,
        "operation":"d".repeat(32),"preload_sequence":"e".repeat(32),"report":[4,1,2,1]});
    for (field, value) in [
        ("action", aterm_json::json!("member-waiting")),
        ("member", aterm_json::json!("f".repeat(32))),
        ("credential_generation", aterm_json::json!(3)),
        ("registration", aterm_json::json!("b".repeat(32))),
    ] {
        let mut wrong = accepted.clone();
        wrong.as_object_mut().unwrap().insert(field.into(), value);
        assert!(!loss.accept(&wrong));
    }
    assert!(loss.accept(&accepted));
    assert_eq!(loss.fired.as_ref(), Some(&accepted));
    assert!(!loss.accept(&accepted), "retry cannot drop another reply");
}

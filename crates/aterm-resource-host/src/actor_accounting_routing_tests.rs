// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn a_valid_owner_reply_survives_scheduling_delay() {
    for (age, expected) in [(3, Ok(true)), (61, Err(mpsc::RecvTimeoutError::Timeout))] {
        let (send, read) = mpsc::sync_channel(1);
        send.send(true).unwrap();
        assert_eq!(
            receive_reply_at(read, Instant::now() - Duration::from_secs(age)),
            expected
        );
    }
}
#[test]
fn accounted_routing_is_exact_and_does_not_relax_legacy_handshake() {
    let hello = aterm_json::json!({"version":3,"role":trust_resource_controller::accounting::PROXY_ROLE,"session":"s-a","attempt":"12121212121212121212121212121212","accounting":trust_resource_controller::accounting::CAPABILITY});
    assert!(proxy_binding_accounted(&hello).is_ok());
    assert!(proxy_binding(&hello).is_err());
    for key in ["version", "role", "session", "attempt", "accounting"] {
        let mut missing = hello.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(proxy_binding_accounted(&missing).is_err());
    }
    let mut wrong = hello.clone();
    wrong
        .as_object_mut()
        .unwrap()
        .insert("accounting".into(), "unknown".into());
    assert!(proxy_binding_accounted(&wrong).is_err());
    let mut extra = hello;
    extra
        .as_object_mut()
        .unwrap()
        .insert("extra".into(), 1.into());
    assert!(proxy_binding_accounted(&extra).is_err());
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// An Allow for a fresh synthetic Mac, signed with a synthetic writer seed.
fn synthetic_allowed(root: &std::path::Path) -> Allowed {
    let joiner = Paths::for_home(&root.join("joiner"));
    machine_identity(&joiner).expect("identity");
    let request = crate::commons::bootstrap::Device::open(&joiner)
        .expect("device")
        .expect("an identity")
        .request([1; 16], [2; 32])
        .expect("request");
    Allowed::sign(request.binding(), "m10", &[7; 32]).expect("signed")
}

/// A member whose state a vault pass holds past the wait says so; it does not claim to hold
/// no vault, which sent the person looking for a vault that was there.
///
/// NEGATIVE CONTROL: map a held lock to `none()` as before (the `Err(if …)` arm returning
/// `none()` alone): the busy assertion fails with "this Mac holds no applied vault".
#[test]
fn a_member_a_pass_holds_says_it_is_busy_not_that_it_holds_no_vault() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let allowed = synthetic_allowed(root.path());
    let member = Paths::for_home(&root.path().join("member"));

    let never = handover_bundle_until(&member, &allowed, std::time::Instant::now(), None)
        .expect_err("a Mac that never joined has nothing to hand");
    assert!(
        never.to_string().contains("holds no applied vault"),
        "{never}"
    );

    let pass = StateLock::take(&member).expect("a pass holds the state");
    let busy = handover_bundle_until(&member, &allowed, std::time::Instant::now(), None)
        .expect_err("the pass still holds the state at the deadline");
    assert!(busy.to_string().contains("still holds its state"), "{busy}");
    assert!(!busy.to_string().contains("no applied vault"), "{busy}");
    drop(pass);
}

#[test]
fn write_from_bundle_rejects_reserved_entries_cli_logins_and_invalid_server_urls() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let allowed = synthetic_allowed(root.path());
    let joiner = Paths::for_home(&root.path().join("joiner"));

    // 1. Reserved entry in `Handed::Files` must be rejected before writing config.
    let reserved_files = text::render_everyone_locker(&[Entry {
        name: crate::SWARM_ROSTER_ENTRY.to_owned(),
        value: crate::SecretBytes::from(&b"members = []\n"[..]),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    }]);
    let bad_reserved = Bundle {
        vault: "test".to_owned(),
        signer: [9; 32],
        followed: Vec::new(),
        servers: vec!["http://127.0.0.1:9000/v1/test".to_owned()],
        allowed: allowed.clone(),
        handed: Handed::Files(reserved_files),
    };
    assert!(write_from_bundle(&joiner, &bad_reserved).is_err());
    assert!(config(&joiner).unwrap().is_none());

    // 2. CLI login entry in `Handed::Files` must be rejected before writing config.
    let cli_files = text::render_everyone_locker(&[Entry {
        name: "~/.claude/.credentials.json".to_owned(),
        value: crate::SecretBytes::from(&b"{}"[..]),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    }]);
    let bad_cli = Bundle {
        vault: "test".to_owned(),
        signer: [9; 32],
        followed: Vec::new(),
        servers: vec!["http://127.0.0.1:9000/v1/test".to_owned()],
        allowed: allowed.clone(),
        handed: Handed::Files(cli_files),
    };
    assert!(write_from_bundle(&joiner, &bad_cli).is_err());
    assert!(config(&joiner).unwrap().is_none());

    // 3. Non-http(s) server URL must be rejected.
    let clean_files = text::render_everyone_locker(&[Entry {
        name: "api_token".to_owned(),
        value: crate::SecretBytes::from(&b"secret"[..]),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    }]);
    let bad_url = Bundle {
        vault: "test".to_owned(),
        signer: [9; 32],
        followed: Vec::new(),
        servers: vec!["file:///tmp/evil".to_owned()],
        allowed: allowed.clone(),
        handed: Handed::Files(clean_files.clone()),
    };
    assert!(write_from_bundle(&joiner, &bad_url).is_err());
    assert!(config(&joiner).unwrap().is_none());

    // 4. Positive control: valid `[everyone]` file bundle succeeds.
    let good = Bundle {
        vault: "test".to_owned(),
        signer: [9; 32],
        followed: Vec::new(),
        servers: vec!["http://127.0.0.1:9000/v1/test".to_owned()],
        allowed,
        handed: Handed::Files(clean_files),
    };
    write_from_bundle(&joiner, &good).expect("clean bundle accepted");
    assert!(config(&joiner).unwrap().is_some());
}

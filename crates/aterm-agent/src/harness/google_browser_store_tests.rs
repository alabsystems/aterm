// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn make_identity(email: &str, reference: &str) -> Identity {
    super::super::google_identity::Registry::parse(&format!("[[identities]]\nid='one'\nemail='{email}'\ncredential_ref='{reference}'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='one'\nexpected_email='{email}'\n")).unwrap().resolve("fixture", "one").unwrap().0.clone()
}

#[test]
fn encrypted_snapshot_survives_restart_but_refuses_substitution_and_damage() {
    let home = aterm_tempfile::tempdir().unwrap();
    let state = aterm_tempfile::tempdir().unwrap();
    let identity = make_identity("one@example.com", "registered-one");
    let claim = identity.claim(state.path()).unwrap();
    let path = claim.profile().join(FILE);
    assert!(load(home.path(), &identity, &claim).unwrap().is_none());
    let cookies = b"[{\"name\":\"fixture\",\"value\":\"never-plaintext-on-disk\"}]";
    save(home.path(), &identity, &claim, cookies).unwrap();
    let mut ciphertext = std::fs::read(&path).unwrap();
    assert!(
        !ciphertext
            .windows(b"never-plaintext-on-disk".len())
            .any(|bytes| bytes == b"never-plaintext-on-disk")
    );
    drop(claim);
    let claim = identity.claim(state.path()).unwrap();
    assert_eq!(
        load(home.path(), &identity, &claim)
            .unwrap()
            .unwrap()
            .as_slice(),
        cookies
    );
    let other = make_identity("other@example.com", "registered-one");
    assert!(load(home.path(), &other, &claim).is_err());
    let changed = make_identity("one@example.com", "registered-replacement");
    assert!(load(home.path(), &changed, &claim).is_err());
    let wrong_machine = aterm_tempfile::tempdir().unwrap();
    aterm_secrets::client::machine_identity(&aterm_secrets::client::Paths::for_home(
        wrong_machine.path(),
    ))
    .unwrap();
    assert!(load(wrong_machine.path(), &identity, &claim).is_err());
    *ciphertext.last_mut().unwrap() ^= 1;
    std::fs::write(&path, ciphertext).unwrap();
    assert!(load(home.path(), &identity, &claim).is_err());
}

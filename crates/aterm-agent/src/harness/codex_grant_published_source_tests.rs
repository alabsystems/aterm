// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn identical_credentials_and_same_generation_cannot_retarget_owned_source() {
    let fixture = crate::harness::codex_enrollment::NativeRowRecoveryFixture::new(&"b".repeat(64));
    fixture.retire_originals().unwrap();
    let applied = fixture.publish_first().unwrap();
    assert_eq!(applied.phase, codex_enrollment::Phase::Applied);
    let env = fixture.env();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let (logins, warning) = store.load_logins(env.now).unwrap();
    assert!(warning.is_none());
    let request = &fixture.requests()[0];
    let binding = logins
        .enrollment
        .published_grant(&request.label, applied.generation.unwrap())
        .unwrap();
    let staging = logins.enrollment.catalogue_staging(request).unwrap();
    let original_auth = std::fs::read(staging.directory.join("auth.json")).unwrap();
    let original_logins = std::fs::read(store.logins_file()).unwrap();
    let copied = env.state.join("copied-source-is-not-authority");
    platform::ensure_private_dir(&copied).unwrap();
    platform::write_private(&copied, "auth.json", &original_auth).unwrap();
    platform::write_private(
        &copied,
        "auth.json.aterm-owner",
        b"ATERM-CODEX-AUTH-OWNER 1\n",
    )
    .unwrap();
    let lifetime = crate::harness::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    for rewrite_fingerprint in [false, true] {
        let mut changed =
            accounts::Logins::from_json(std::str::from_utf8(&original_logins).unwrap(), env.now)
                .unwrap();
        changed.labels.get_mut(&request.label).unwrap().dir = Some(copied.to_str().unwrap().into());
        assert!(changed.enrollment.authenticates_published_grant(&binding));
        let fingerprint =
            crate::harness::codex_native_row::binding_digest(&changed, &request.label).unwrap();
        let mut value: aterm_json::Value = aterm_json::from_str(&changed.to_json()).unwrap();
        if rewrite_fingerprint {
            value
                .as_object_mut()
                .and_then(|object| object.get_mut("enrollment"))
                .and_then(aterm_json::Value::as_object_mut)
                .and_then(|book| book.get_mut(request.id.as_str()))
                .and_then(aterm_json::Value::as_object_mut)
                .and_then(|record| record.get_mut("row_publication"))
                .and_then(aterm_json::Value::as_object_mut)
                .unwrap()
                .insert("binding_digest".into(), fingerprint.into());
        }
        // Deliberate hostile canonical metadata substitution, not a writer
        // authority or an alternate publication. The issuer MAC is intact.
        platform::write_private(
            &store.dir,
            accounts::LOGINS_FILE,
            &aterm_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let (reloaded, warning) = store.load_logins(env.now).unwrap();
        assert!(warning.is_none());
        assert!(reloaded.enrollment.authenticates_published_grant(&binding));
        let mut entered = false;
        assert!(
            with_maintenance_owner(env, &binding, &scope, |_| {
                entered = true;
                Ok(())
            })
            .is_err()
        );
        assert!(!entered);
        assert!(
            !copied.join(codex_app_login::AUTH_LOCK_FILE).exists(),
            "unauthenticated destination must be refused before credential custody opens"
        );
        assert!(catalogue::inventory_current_for_test(env).is_err());
        assert_eq!(
            std::fs::read(copied.join("auth.json")).unwrap(),
            original_auth
        );
        assert_eq!(
            std::fs::read(staging.directory.join("auth.json")).unwrap(),
            original_auth
        );
    }
    platform::write_private(&store.dir, accounts::LOGINS_FILE, &original_logins).unwrap();
    assert!(with_maintenance_owner(env, &binding, &scope, |_| Ok(())).is_ok());
    assert!(catalogue::inventory_current_for_test(env).is_ok());
}

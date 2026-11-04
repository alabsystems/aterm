// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;

const EMAIL: &str = "approved@example.test";

fn document(email: &str, user: &str) -> Value {
    let token = super::super::tests::make_jwt(&json!({
        "email":email, "exp":4102444800_u64,
        "https://api.openai.com/auth":{
            "chatgpt_account_id":"account-1", "chatgpt_user_id":user,
            "chatgpt_plan_type":"pro"
        }
    }));
    json!({"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{
        "access_token":token,"id_token":token,"account_id":"account-1",
        "oauth_provenance":{"issuer":DEFAULT_ISSUER,"client_id":CLIENT_ID}
    }})
}

fn write(home: &Path, value: &Value) {
    let path = home.join(AUTH_FILE);
    std::fs::write(&path, aterm_json::to_vec(value).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn accepted() -> Reading {
    Reading::Usage(Usage {
        sample: Default::default(),
        walls: Vec::new(),
    })
}

/// Unix only, where `codex_app_login::shared::snapshot` reads owner-private `auth.json` bytes;
/// off Unix `shared_codex_refuses_verification_where_private_evidence_is_unavailable` holds
/// the refusal.
#[cfg(unix)]
#[test]
fn shared_codex_accepts_provider_verified_access_email_without_an_id_token() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let mut value = document(EMAIL, "user-1");
    value
        .as_object_mut()
        .unwrap()
        .get_mut("tokens")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("id_token");
    write(temp.path(), &value);
    let requested = Cell::new(0);
    let proof = verify_with(temp.path(), EMAIL, || {
        requested.set(requested.get() + 1);
        accepted()
    })
    .unwrap()
    .unwrap();
    assert_eq!(requested.get(), 1);
    assert_eq!(proof.email(), EMAIL);
    assert_eq!(proof.principal(), principal("account-1", "user-1").unwrap());
}

#[test]
fn shared_codex_refuses_unverified_id_email_when_access_token_has_no_email() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let mut value = document(EMAIL, "user-1");
    let access = super::super::tests::make_jwt(&json!({
        "exp":4102444800_u64,
        "https://api.openai.com/auth":{
            "chatgpt_account_id":"account-1", "chatgpt_user_id":"user-1",
            "chatgpt_plan_type":"pro"
        }
    }));
    value
        .as_object_mut()
        .unwrap()
        .get_mut("tokens")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("access_token".into(), Value::String(access));
    write(temp.path(), &value);
    let requested = Cell::new(0);
    let result = verify_with(temp.path(), EMAIL, || {
        requested.set(requested.get() + 1);
        accepted()
    });
    assert!(result.is_err());
    assert_eq!(requested.get(), 0);
}

#[test]
fn shared_codex_refuses_disagreeing_account_or_user_despite_access_email() {
    let temp = aterm_tempfile::tempdir().unwrap();
    for case in 0..3 {
        let mut value = document(EMAIL, "user-1");
        let tokens = value
            .as_object_mut()
            .unwrap()
            .get_mut("tokens")
            .unwrap()
            .as_object_mut()
            .unwrap();
        if case == 0 {
            tokens.insert("account_id".into(), Value::String("account-other".into()));
        } else {
            let id = super::super::tests::make_jwt(&json!({
                "email":EMAIL, "exp":4102444800_u64,
                "https://api.openai.com/auth":{
                    "chatgpt_account_id":if case == 1 { "account-other" } else { "account-1" },
                    "chatgpt_user_id":if case == 2 { "user-other" } else { "user-1" }
                }
            }));
            tokens.insert("id_token".into(), Value::String(id));
        }
        write(temp.path(), &value);
        let requested = Cell::new(false);
        assert!(
            verify_with(temp.path(), EMAIL, || {
                requested.set(true);
                accepted()
            })
            .is_err()
        );
        assert!(!requested.get());
    }
}

/// Unix only, where `codex_app_login::shared::snapshot` reads owner-private `auth.json` bytes;
/// off Unix `shared_codex_refuses_verification_where_private_evidence_is_unavailable` holds
/// the refusal.
#[cfg(unix)]
#[test]
fn shared_codex_does_not_take_subscription_from_an_unverified_id_token() {
    let temp = aterm_tempfile::tempdir().unwrap();
    for case in 0..3 {
        let mut value = document(EMAIL, "user-1");
        let mut claims = json!({
            "email":EMAIL, "exp":4102444800_u64,
            "https://api.openai.com/auth":{
                "chatgpt_account_id":"account-1", "chatgpt_user_id":"user-1"
            }
        });
        if case == 0 {
            claims
                .as_object_mut()
                .unwrap()
                .get_mut("https://api.openai.com/auth")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("chatgpt_plan_type".into(), Value::String("pro".into()));
        }
        let tokens = value
            .as_object_mut()
            .unwrap()
            .get_mut("tokens")
            .unwrap()
            .as_object_mut()
            .unwrap();
        tokens.insert(
            "access_token".into(),
            Value::String(super::super::tests::make_jwt(&claims)),
        );
        if case == 2 {
            tokens.remove("id_token");
        } else {
            let id = super::super::tests::make_jwt(&json!({
                "email":EMAIL, "exp":4102444800_u64,
                "https://api.openai.com/auth":{
                    "chatgpt_account_id":"account-1", "chatgpt_user_id":"user-1",
                    "chatgpt_plan_type":"enterprise"
                }
            }));
            tokens.insert("id_token".into(), Value::String(id));
        }
        write(temp.path(), &value);
        let requested = Cell::new(0);
        let proof = verify_with(temp.path(), EMAIL, || {
            requested.set(requested.get() + 1);
            accepted()
        })
        .unwrap()
        .unwrap();
        assert_eq!(requested.get(), 1);
        assert_eq!(proof.email(), EMAIL);
        assert_eq!(proof.plan(), if case == 0 { Some("pro") } else { None });
    }
}

/// Unix only, where `codex_app_login::shared::snapshot` reads owner-private `auth.json` bytes;
/// off Unix `shared_codex_refuses_verification_where_private_evidence_is_unavailable` holds
/// the refusal.
#[cfg(unix)]
#[test]
fn shared_codex_proof_requires_provider_acceptance_and_exact_private_snapshot() {
    let temp = aterm_tempfile::tempdir().unwrap();
    write(temp.path(), &document(EMAIL, "user-1"));
    assert!(verify_with(temp.path(), EMAIL, || Reading::Unread).is_err());
    let proof = verify_with(temp.path(), EMAIL, accepted).unwrap().unwrap();
    assert_eq!(proof.email(), EMAIL);
    assert_eq!(proof.principal(), principal("account-1", "user-1").unwrap());
    assert_ne!(proof.principal(), principal("account-1", "user-2").unwrap());
    assert_eq!(proof.plan(), Some("pro"));
    proof.check_current().unwrap();
    let mut changed = document(EMAIL, "user-1");
    changed
        .as_object_mut()
        .unwrap()
        .get_mut("tokens")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert(
            "refresh_token".into(),
            Value::String("changed-synthetic-token".into()),
        );
    write(temp.path(), &changed);
    assert!(proof.check_current().is_err());
}

#[test]
fn shared_codex_rejects_wrong_identity_or_issuer_before_any_provider_request() {
    let temp = aterm_tempfile::tempdir().unwrap();
    for case in 0..3 {
        let mut value = document(
            if case == 0 {
                "other@example.test"
            } else {
                EMAIL
            },
            "user-1",
        );
        if case == 1 {
            value
                .as_object_mut()
                .unwrap()
                .get_mut("tokens")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .get_mut("oauth_provenance")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(
                    "issuer".into(),
                    Value::String("https://other.invalid".into()),
                );
        }
        if case == 2 {
            value
                .as_object_mut()
                .unwrap()
                .insert("OPENAI_API_KEY".into(), Value::String("synthetic".into()));
        }
        write(temp.path(), &value);
        let requested = Cell::new(false);
        assert!(
            verify_with(temp.path(), EMAIL, || {
                requested.set(true);
                accepted()
            })
            .is_err()
        );
        assert!(!requested.get());
    }
}

/// Unix only, where `codex_app_login::shared::snapshot` reads owner-private `auth.json` bytes;
/// off Unix `shared_codex_refuses_verification_where_private_evidence_is_unavailable` holds
/// the refusal.
#[cfg(unix)]
#[test]
fn shared_codex_does_not_attribute_a_concurrent_or_refreshed_file_to_prior_request() {
    let temp = aterm_tempfile::tempdir().unwrap();
    write(temp.path(), &document(EMAIL, "user-1"));
    assert!(
        verify_with(temp.path(), EMAIL, || {
            write(temp.path(), &document(EMAIL, "user-2"));
            accepted()
        })
        .is_err()
    );
    assert_eq!(
        verify_with(temp.path(), EMAIL, accepted)
            .unwrap()
            .unwrap()
            .principal(),
        principal("account-1", "user-2").unwrap()
    );
}

/// Off Unix `codex_app_login::shared::snapshot` refuses staged `auth.json` verification before
/// making any provider request, while portable token identity rules still reject invalid claims.
#[cfg(not(unix))]
#[test]
fn shared_codex_refuses_verification_where_private_evidence_is_unavailable() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let valid = document(EMAIL, "user-1");
    assert_eq!(
        identity(&aterm_json::to_vec(&valid).unwrap(), EMAIL)
            .unwrap()
            .0,
        EMAIL
    );
    assert!(identity(&aterm_json::to_vec(&valid).unwrap(), "other@example.test").is_err());
    write(temp.path(), &valid);
    let err = match verify_with(temp.path(), EMAIL, || {
        panic!("no provider request off Unix")
    }) {
        Ok(_) => panic!("expected off-Unix refusal"),
        Err(err) => err,
    };
    assert_eq!(
        err,
        ObservationError::Unavailable(
            "private shared-login evidence is unavailable on this platform".into()
        )
    );
    assert_eq!(local_principal(temp.path(), EMAIL), None);
}

#[test]
fn shared_codex_private_file_policy_and_absence_are_not_authentication() {
    let temp = aterm_tempfile::tempdir().unwrap();
    assert!(
        verify_with(temp.path(), EMAIL, || panic!("no credential"))
            .unwrap()
            .is_none()
    );
    write(temp.path(), &document(EMAIL, "user-1"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            temp.path().join(AUTH_FILE),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(verify_with(temp.path(), EMAIL, || panic!("public credential")).is_err());
    }
}

/// Unix only: these observations require owner-private credential snapshots.
#[cfg(unix)]
#[test]
fn shared_codex_preserves_unread_and_rotated_attempts_without_reauthentication() {
    let temp = aterm_tempfile::tempdir().unwrap();
    for reading in [Reading::Unread, Reading::SignedOut, accepted()] {
        let initial = document(EMAIL, "user-1");
        write(temp.path(), &initial);
        let result = verify_with(temp.path(), EMAIL, || reading.clone());
        match &reading {
            Reading::Unread => assert!(matches!(result, Err(ObservationError::Unavailable(_)))),
            Reading::SignedOut => {
                assert!(matches!(result, Err(ObservationError::RecoveryRequired(_))))
            }
            Reading::Usage(_) => assert!(result.unwrap().is_some()),
        }
        let mut rotated = initial.clone();
        rotated.as_object_mut().unwrap().insert(
            "last_refresh".into(),
            Value::String("2026-11-04T12:00:00Z".into()),
        );
        let result = verify_with(temp.path(), EMAIL, || {
            write(temp.path(), &rotated);
            reading.clone()
        });
        assert!(matches!(result, Err(ObservationError::Unavailable(_))));
        let proof = verify_with(temp.path(), EMAIL, accepted).unwrap().unwrap();
        assert_eq!(proof.principal(), principal("account-1", "user-1").unwrap());
        proof.check_current().unwrap();
        assert_eq!(
            std::fs::read(temp.path().join(AUTH_FILE)).unwrap(),
            aterm_json::to_vec(&rotated).unwrap()
        );
    }
}

#[cfg(unix)]
#[test]
fn shared_codex_broken_credential_link_is_unavailable_instead_of_missing() {
    let temp = aterm_tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(temp.path().join("absent"), temp.path().join(AUTH_FILE)).unwrap();
    let result = verify_with(temp.path(), EMAIL, || {
        panic!("unsafe evidence must not reach provider")
    });
    assert!(matches!(result, Err(ObservationError::Unavailable(_))));
}

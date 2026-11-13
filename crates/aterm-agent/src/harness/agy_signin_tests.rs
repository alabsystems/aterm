// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn temporary_google_verification_keeps_the_same_claimed_candidate() {
    use super::super::{google_browser, google_browser_store, google_profile};
    use std::cell::Cell;
    const OLD: &[u8] =
        br#"[{"name":"fixture","value":"old","domain":".google.com","path":"/","secure":true}]"#;
    const NEW: &[u8] =
        br#"[{"name":"fixture","value":"new","domain":".google.com","path":"/","secure":true}]"#;
    for scenario in 0..10 {
        let home = aterm_tempfile::tempdir().unwrap();
        let state = aterm_tempfile::tempdir().unwrap();
        let profile = aterm_tempfile::tempdir().unwrap();
        aterm_secrets::fsx::write_atomic(&state.path().join("google-identities.toml"), b"[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n", 0o600).unwrap();
        let identity = registry(state.path())
            .unwrap()
            .resolve("agy", "one")
            .unwrap()
            .0
            .clone();
        let claim = identity.claim(state.path()).unwrap();
        let snapshot = claim.profile().join("session.age");
        google_browser_store::save(home.path(), &identity, &claim, OLD).unwrap();
        let previous = std::fs::read(&snapshot).unwrap();
        drop(claim);
        let mut grant =
            publication_fixture(state.path(), profile.path(), "one", "person@example.com").grant;
        let native_path = grant.path().to_path_buf();
        let original = std::fs::read(&native_path).unwrap();
        grant.native.session = Some(google_browser::pending_fixture(
            &identity,
            state.path(),
            home.path(),
            NEW,
        ));
        let clock = Cell::new(Instant::now());
        let deadline = clock.get()
            + if scenario == 8 {
                Duration::from_millis(500)
            } else {
                Duration::from_secs(60)
            };
        let mut calls = 0;
        let mut delays = Vec::new();
        let mut token_seen = None;
        let result = grant.verify_with_retry(
            || deadline.saturating_duration_since(clock.get()),
            |token, expected, wait| {
                calls += 1;
                assert_eq!(expected, "person@example.com");
                assert!(!wait.is_zero() && wait <= crate::harness::google_profile::REQUEST_HANG);
                assert_eq!(std::fs::read(&native_path).unwrap(), original);
                assert_eq!(std::fs::read(&snapshot).unwrap(), previous);
                assert!(
                    identity.claim(state.path()).is_err(),
                    "Google claim released between requests"
                );
                assert!(
                    super::super::browser_window::ProfileLock::claim(profile.path()).is_err(),
                    "native claim released between requests"
                );
                if let Some(previous_token) = &token_seen {
                    assert_eq!(previous_token, token);
                }
                token_seen = Some(token.to_owned());
                match scenario {
                    0 if calls == 2 => {}
                    1 if calls == 3 => {}
                    3 => return Err(google_profile::Error::CredentialRejected),
                    4 => return Err(google_profile::Error::Refused("wrong account")),
                    9 if calls == 2 => return Err(google_profile::Error::CredentialRejected),
                    _ => return Err(google_profile::Error::Unavailable),
                }
                Ok(google_profile::GoogleProfile {
                    email: expected.into(),
                    principal: [1; 32],
                })
            },
            |delay| {
                delays.push(delay);
                clock.set(if scenario == 7 {
                    deadline
                } else {
                    clock.get() + delay
                });
                if scenario == 5 {
                    aterm_secrets::fsx::write_atomic(
                        &state.path().join("google-identities.toml"),
                        b"changed",
                        0o600,
                    )
                    .unwrap();
                } else if scenario == 6 {
                    aterm_secrets::fsx::write_atomic(&native_path, b"changed", 0o600).unwrap();
                }
            },
        );
        assert_eq!(
            calls,
            match scenario {
                0 | 9 => 2,
                1 | 2 => 3,
                _ => 1,
            },
            "scenario {scenario}"
        );
        assert_eq!(
            delays,
            match scenario {
                1 | 2 => vec![Duration::from_secs(1), Duration::from_secs(2)],
                0 | 5 | 6 | 7 | 9 => vec![Duration::from_secs(1)],
                _ => vec![],
            },
            "scenario {scenario}"
        );
        if scenario <= 1 {
            assert!(result.unwrap().path().is_ok());
            let claim = identity.claim(state.path()).unwrap();
            assert_eq!(
                google_browser_store::load(home.path(), &identity, &claim)
                    .unwrap()
                    .unwrap()
                    .as_slice(),
                NEW
            );
        } else {
            let error = result.err().expect("failure must not publish a proof");
            if scenario == 2 || scenario == 8 {
                assert_eq!(error, google_profile::Error::Unavailable);
            }
            if scenario == 3 || scenario == 9 {
                assert_eq!(error, google_profile::Error::CredentialRejected);
            }
            assert_eq!(std::fs::read(&snapshot).unwrap(), previous);
            assert!(identity.claim(state.path()).is_ok());
            assert!(super::super::browser_window::ProfileLock::claim(profile.path()).is_ok());
        }
    }
}

#[test]
fn native_google_pending_session_commits_only_after_current_provider_proof() {
    use super::super::{google_browser, google_browser_store};
    const OLD: &[u8] = br#"[{"name":"fixture","value":"old-session","domain":".google.com","path":"/","secure":true}]"#;
    const NEW: &[u8] = br#"[{"name":"fixture","value":"renewed-session","domain":".google.com","path":"/","secure":true}]"#;
    for scenario in 0..7 {
        let home = aterm_tempfile::tempdir().unwrap();
        let state = aterm_tempfile::tempdir().unwrap();
        let profile = aterm_tempfile::tempdir().unwrap();
        aterm_secrets::fsx::write_atomic(&state.path().join("google-identities.toml"), b"[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n", 0o600).unwrap();
        let identity = registry(state.path())
            .unwrap()
            .resolve("agy", "one")
            .unwrap()
            .0
            .clone();
        let claim = identity.claim(state.path()).unwrap();
        let snapshot = claim.profile().join("session.age");
        google_browser_store::save(home.path(), &identity, &claim, OLD).unwrap();
        let previous = std::fs::read(&snapshot).unwrap();
        drop(claim);
        let candidate_identity = if scenario == 6 {
            super::super::google_identity::Registry::parse("[[identities]]\nid='another-namespace'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='another-namespace'\nexpected_email='person@example.com'\n").unwrap().resolve("agy", "one").unwrap().0.clone()
        } else {
            identity.clone()
        };
        let mut grant =
            publication_fixture(state.path(), profile.path(), "one", "person@example.com").grant;
        grant.native.session = Some(google_browser::pending_fixture(
            &candidate_identity,
            state.path(),
            home.path(),
            NEW,
        ));
        let deadline = WakeDeadline::after(if scenario == 5 {
            Duration::ZERO
        } else {
            Duration::from_secs(60)
        });
        let result = grant.verify_using(deadline, |_, expected, _| {
            assert_ne!(scenario, 5, "expired verification contacted the provider");
            assert_eq!(expected, "person@example.com");
            assert_eq!(
                std::fs::read(&snapshot).unwrap(),
                previous,
                "unverified cookies reached disk"
            );
            assert!(
                candidate_identity.claim(state.path()).is_err(),
                "identity ownership ended before provider proof"
            );
            match scenario {
                1 => aterm_secrets::fsx::write_atomic(
                    &profile
                        .path()
                        .join(".gemini/antigravity-cli/antigravity-oauth-token"),
                    b"changed",
                    0o600,
                )
                .unwrap(),
                2 => aterm_secrets::fsx::write_atomic(
                    &state.path().join("google-identities.toml"),
                    b"changed",
                    0o600,
                )
                .unwrap(),
                3 => return Err(super::super::google_profile::Error::CredentialRejected),
                _ => {}
            }
            Ok(super::super::google_profile::GoogleProfile {
                email: if scenario == 4 {
                    "another@example.com"
                } else {
                    expected
                }
                .into(),
                principal: [1; 32],
            })
        });
        if scenario == 0 {
            let verified = result.unwrap();
            assert!(verified.path().is_ok());
            let bytes = std::fs::read(&snapshot).unwrap();
            assert_ne!(bytes, previous);
            assert!(
                !bytes
                    .windows(b"renewed-session".len())
                    .any(|part| part == b"renewed-session")
            );
            let claim = identity.claim(state.path()).unwrap();
            assert_eq!(
                google_browser_store::load(home.path(), &identity, &claim)
                    .unwrap()
                    .unwrap()
                    .as_slice(),
                NEW
            );
        } else {
            assert!(result.is_err());
            assert_eq!(std::fs::read(&snapshot).unwrap(), previous);
            assert!(candidate_identity.claim(state.path()).is_ok());
            if scenario == 6 {
                let claim = candidate_identity.claim(state.path()).unwrap();
                assert!(!claim.profile().join("session.age").exists());
            }
        }
    }
}

/// A GRANT VERIFICATION ITS HOST SLEPT PAST STARTS NO NEW REQUEST (KI-1105-14): an hour's
/// verification whose host slept two hours during its first account check, which Google
/// could not answer, ends then as unavailable, with no retry. NEGATIVE CONTROL: on the clock
/// that stops in sleep alone the hour was left, and it paused (a second, then two) and asked
/// twice more.
#[cfg(unix)]
#[test]
fn a_grant_verification_its_host_slept_past_starts_no_new_request() {
    let state = aterm_tempfile::tempdir().unwrap();
    let profile = aterm_tempfile::tempdir().unwrap();
    aterm_secrets::fsx::write_atomic(&state.path().join("google-identities.toml"), b"[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n", 0o600).unwrap();
    let grant =
        publication_fixture(state.path(), profile.path(), "one", "person@example.com").grant;
    let _seam = super::super::wake_deadline::SleepSeam::hold();
    let deadline = WakeDeadline::after(Duration::from_secs(3600));
    let mut calls = 0;
    let result = grant.verify_using(deadline, |_, _, _| {
        calls += 1;
        super::super::wake_deadline::host_slept(Duration::from_secs(2 * 3600));
        Err(super::super::google_profile::Error::Unavailable)
    });
    assert_eq!(
        result.err(),
        Some(super::super::google_profile::Error::Unavailable)
    );
    assert_eq!(calls, 1, "no request after the sleep");
}

#[test]
fn native_google_subject_requires_guarded_scoped_identity() {
    use super::super::accounts;
    let root = aterm_tempfile::tempdir().unwrap();
    let mut logins = accounts::Logins::default();
    logins.labels.insert(
        "one".into(),
        accounts::Binding {
            dir: Some(root.path().to_str().unwrap().into()),
            ..Default::default()
        },
    );
    accounts::note_google_identity(&mut logins, "one", "person@example.com", [1; 32]);
    let serialized = logins.to_json();
    let restored = accounts::Logins::from_json(&serialized, 2).unwrap();
    assert_eq!(restored.labels["one"].google_principal, Some([1; 32]));
    assert!(accounts::require_google_principal(&restored, "one", [1; 32]).is_ok());
    assert!(accounts::require_google_principal(&restored, "one", [2; 32]).is_err());
    for case in 0..3 {
        let mut document: aterm_json::Value = aterm_json::from_str(&serialized).unwrap();
        if case == 0 {
            document
                .as_object_mut()
                .unwrap()
                .insert("schema".into(), aterm_json::Value::from(3u64));
        } else {
            let binding = document
                .as_object_mut()
                .unwrap()
                .get_mut("labels")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .get_mut("one")
                .unwrap()
                .as_object_mut()
                .unwrap();
            if case == 1 {
                binding.insert(
                    "google_principal".into(),
                    aterm_json::Value::from("invalid"),
                );
            } else {
                binding.insert("proved_dir".into(), aterm_json::Value::Null);
            }
        }
        assert!(accounts::Logins::from_json(&document.to_string(), 2).is_err());
    }
    accounts::note_proof(&mut logins, "one", accounts::Proof::Unchecked);
    let revoked = accounts::Logins::from_json(&logins.to_json(), 2).unwrap();
    assert_eq!(revoked.labels["one"].google_principal, None);
}

#[test]
fn native_google_checkpoint_retains_subject_and_exact_grant_bytes() {
    use super::super::accounts;
    use crate::ai::connector::Cli;
    let state = aterm_tempfile::tempdir().unwrap();
    let staging = aterm_tempfile::tempdir().unwrap();
    aterm_secrets::fsx::write_atomic(
        &state.path().join("google-identities.toml"),
        b"[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n",
        0o600,
    ).unwrap();
    let verified = publication_fixture(state.path(), staging.path(), "one", "person@example.com");
    let logins = accounts::Logins::default();
    let actual = std::fs::canonicalize(staging.path()).unwrap();
    assert!(
        accounts::write_renewal_checkpoint_for(
            Cli::Agy,
            None,
            "one",
            "person@example.com",
            staging.path().to_str().unwrap(),
            None,
            Some(accounts::Proof::Status),
            1,
            &logins
        )
        .is_err()
    );
    accounts::write_google_renewal_checkpoint(
        "one",
        staging.path().to_str().unwrap(),
        &verified,
        1,
        &logins,
    )
    .unwrap();
    let read = || accounts::read_renewal_checkpoint_for(Cli::Agy, staging.path(), &actual, 2);
    let checkpoint = read().unwrap();
    assert_eq!(checkpoint.google_principal, Some([1; 32]));
    assert_eq!(
        checkpoint.google_grant_digest,
        Some(verified.digest().unwrap())
    );
    let row = accounts::Row {
        label: "one".into(),
        email: Some("person@example.com".into()),
        ..Default::default()
    };
    assert!(accounts::checkpoint_identity_matches(
        Cli::Agy,
        &row,
        &checkpoint
    ));
    assert!(accounts::checkpoint_current(
        &row,
        &accounts::State::default(),
        &logins,
        &checkpoint,
        Some(state.path())
    ));
    let token = verified.path().unwrap().to_owned();
    aterm_secrets::fsx::write_atomic(&token, b"replaced after checkpoint", 0o600).unwrap();
    assert!(read().is_none());
    assert!(!accounts::checkpoint_current(
        &row,
        &accounts::State::default(),
        &logins,
        &checkpoint,
        Some(state.path())
    ));
    assert!(verified.digest().is_err());
}

#[test]
fn native_token_requires_current_nested_schema_not_cached_identity() {
    let valid =
        br#"{"auth_method":"consumer","token":{"access_token":"synthetic-token"},"id_token_email":"wrong@example.com"}"#;
    assert_eq!(
        native_access_token(valid).unwrap().as_str(),
        Some("synthetic-token")
    );
    for invalid in [
        br#"{"auth_method":"consumer","access_token":"synthetic-token"}"#.as_slice(),
        br#"{"id_token_email":"person@example.com","id_token":"synthetic-jwt"}"#,
        br#"{"auth_method":"consumer","token":{"access_token":""}}"#,
        br#"{"auth_method":"consumer","token":{"access_token":"bad\r\nheader"}}"#,
        br#"{"auth_method":"gcp","token":{"access_token":"synthetic-token"}}"#,
        br#"{"token":{"access_token":"synthetic-token"}}"#,
    ] {
        assert!(native_access_token(invalid).is_err());
    }
}

#[test]
fn account_proof_refuses_replaced_grant_and_changed_binding() {
    let state = aterm_tempfile::tempdir().unwrap();
    let profile = aterm_tempfile::tempdir().unwrap();
    let text = "[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n";
    let config = state.path().join("google-identities.toml");
    aterm_secrets::fsx::write_atomic(&config, text.as_bytes(), 0o600).unwrap();
    let revision = registry(state.path())
        .unwrap()
        .binding_revision("agy", "one");
    let gemini = profile.path().join(".gemini");
    let path = gemini.join("antigravity-cli/antigravity-oauth-token");
    aterm_secrets::fsx::ensure_private_dir(path.parent().unwrap()).unwrap();
    let bytes = br#"{"auth_method":"consumer","token":{"access_token":"synthetic-token"}}"#;
    aterm_secrets::fsx::write_atomic(&path, bytes, 0o600).unwrap();
    // Synthetic proof exercises the post-verification publication guard, not Google login.
    let verified = VerifiedGoogleGrant {
        grant: UnverifiedGoogleGrant {
            native: UnverifiedGrant {
                session: None,
                path: path.clone(),
                program: PathBuf::from("/synthetic/agy"),
                _claim: super::super::browser_window::ProfileLock::claim(profile.path()).unwrap(),
            },
            expected_email: "person@example.com".into(),
            state: state.path().to_owned(),
            account: "one".into(),
            revision,
            recovery: None,
            work: None,
        },
        profile: super::super::google_profile::GoogleProfile {
            email: "person@example.com".into(),
            principal: [1; 32],
        },
        digest: grant_digest(bytes),
    };
    assert!(verified.path().is_ok());
    aterm_secrets::fsx::write_atomic(&path, b"replacement", 0o600).unwrap();
    assert!(verified.path().is_err());
    aterm_secrets::fsx::write_atomic(&path, bytes, 0o600).unwrap();
    assert!(verified.path().is_ok());
    aterm_secrets::fsx::write_atomic(&config, text.replace("person@", "other@").as_bytes(), 0o600)
        .unwrap();
    assert!(verified.path().is_err());
}

const BINDING: &str = "[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n";

/// A bound state root, a published profile and a program file that is never run.
fn published_world() -> (aterm_tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let profile = root.path().join("profile");
    for dir in [&state, &profile] {
        aterm_secrets::fsx::ensure_private_dir(dir).unwrap();
    }
    aterm_secrets::fsx::write_atomic(
        &state.join("google-identities.toml"),
        BINDING.as_bytes(),
        0o600,
    )
    .unwrap();
    let program = root.path().join("agy");
    aterm_secrets::fsx::write_atomic(&program, b"never run", 0o600).unwrap();
    (root, state, profile, program)
}

fn write_grant(profile: &Path, bytes: &[u8]) -> PathBuf {
    let path = super::super::upgrade_agy::profile_grant(profile);
    aterm_secrets::fsx::ensure_private_dir(path.parent().unwrap()).unwrap();
    aterm_secrets::fsx::write_atomic(&path, bytes, 0o600).unwrap();
    path
}

/// The launch check of a published grant answers from this Mac alone (§4.15 rule 6): a grant
/// whose bytes hold no token at all is still a signed-in row, because the check asks only
/// that the file is present and opens, so it holds nothing a provider request could carry,
/// and records no probe.
#[test]
fn a_launch_checks_a_published_grant_locally_without_its_contents() {
    let (_root, state, profile, program) = published_world();
    write_grant(&profile, b"opaque bytes no request can carry");
    let (checked, probes) = super::super::provider_read::captured(|| {
        check_google_published(&program, &state, "one", "person@example.com", &profile)
    });
    assert!(checked.is_ok(), "{:?}", checked.err());
    assert!(probes.is_empty());
}

/// CONTROL: with the launch reading the grant's contents, as the userinfo request it once
/// made had to, the same grant is refused, so the test above holds only because nothing reads
/// them.
#[test]
fn control_a_launch_reading_grant_contents_refuses_the_same_grant() {
    let (_root, state, profile, program) = published_world();
    write_grant(&profile, b"opaque bytes no request can carry");
    let checked = with_launch_reading_grant_contents(|| {
        check_google_published(&program, &state, "one", "person@example.com", &profile)
    });
    assert!(matches!(checked, Err(PublishedRefusal::Refused(_))));
}

#[test]
fn a_published_grant_that_is_absent_or_does_not_open_is_signed_out_here() {
    let (_root, state, profile, program) = published_world();
    let check = || check_google_published(&program, &state, "one", "person@example.com", &profile);
    assert_eq!(check().err(), Some(PublishedRefusal::SignedOut));
    let path = write_grant(&profile, b"");
    assert_eq!(check().err(), Some(PublishedRefusal::SignedOut));
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(check().err(), Some(PublishedRefusal::SignedOut));
    std::fs::remove_dir(&path).unwrap();
    write_grant(&profile, b"grant");
    assert!(check().is_ok());
    assert!(matches!(
        check_google_published(&program, &state, "one", "other@example.com", &profile),
        Err(PublishedRefusal::Refused(_))
    ));
}

#[test]
fn a_published_worker_rebinds_its_profile_and_rechecks_before_admission() {
    let (_root, state, profile, program) = published_world();
    let path = write_grant(&profile, b"grant");
    let gemini = profile.join(".gemini");
    let prepared = check_google_published(&program, &state, "one", "person@example.com", &profile)
        .unwrap()
        .prepare_worker(super::super::worker::Starter {
            exe: PathBuf::from("/inherited/agy"),
            env: vec![
                ("HOME".into(), "/inherited/home".into()),
                ("GEMINI_API_KEY".into(), "synthetic-key".into()),
            ],
            args: Ok(vec![
                "-gemini_dir=/inherited".into(),
                "--app_data_dir".into(),
                "old".into(),
                "--mode".into(),
                "plan".into(),
            ]),
            unset: Vec::new(),
        })
        .unwrap();
    let starter = prepared.starter().unwrap();
    assert_eq!(starter.exe, program);
    assert_eq!(
        starter.args.as_ref().unwrap(),
        &vec![
            "--mode".to_owned(),
            "plan".into(),
            "--gemini_dir".into(),
            gemini.to_str().unwrap().into(),
            "--app_data_dir=antigravity-cli".into()
        ]
    );
    assert!(
        starter
            .env
            .iter()
            .any(|(k, v)| k == "HOME" && v == profile.to_str().unwrap())
    );
    assert!(!starter.env.iter().any(|(k, _)| k == "GEMINI_API_KEY"));
    assert!(starter.unset.contains(&"GEMINI_API_KEY"));
    assert!(super::super::browser_window::ProfileLock::claim(&profile).is_err());
    // agy rewrites its own grant as it renews its token: that is still this row's login.
    aterm_secrets::fsx::write_atomic(&path, b"renewed by agy", 0o600).unwrap();
    assert!(prepared.starter().is_ok());
    std::fs::remove_file(&path).unwrap();
    assert!(prepared.starter().is_err());
    write_grant(&profile, b"grant");
    aterm_secrets::fsx::write_atomic(
        &state.join("google-identities.toml"),
        BINDING.replace("person@", "other@").as_bytes(),
        0o600,
    )
    .unwrap();
    assert!(prepared.starter().is_err());
}

#[test]
fn worker_profile_rebinding_respects_flag_looking_values_and_refuses_unknown_flags() {
    let bind = |flags: &[&str]| {
        super::super::upgrade_agy::bound_worker_flags(
            &flags.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            Path::new("/private/.gemini"),
        )
    };
    let flags = bind(&[
        "--model",
        "--gemini_dir=not-a-profile",
        "--gemini_dir",
        "--mode=not-a-permission",
        "--mode=plan",
    ])
    .unwrap();
    assert_eq!(
        &flags[..3],
        ["--model", "--gemini_dir=not-a-profile", "--mode=plan"]
    );
    for flags in [
        &["--unknown"][..],
        &["--gemini_dir"],
        &["--settings", "{}"],
        &["--", "operand"],
    ] {
        assert!(bind(flags).is_err());
    }
}

fn request(state: &str) -> String {
    let scope = SCOPES.join("%20");
    format!(
        "https://accounts.google.com/o/oauth2/auth?client_id=native-fixture.apps.googleusercontent.com&redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback&response_type=code&access_type=offline&code_challenge_method=S256&code_challenge=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&state={state}&scope={scope}"
    )
}

#[test]
fn google_callback_cannot_cross_native_requests_or_change_its_destination() {
    let first = Authorization::admit(&request("first-state")).unwrap();
    let second = Authorization::admit(&request("second-state")).unwrap();
    let code = first
        .callback("https://antigravity.google/oauth-callback?code=fixture-code&state=first-state")
        .unwrap();
    assert_eq!(code.value.as_str(), Some("fixture-code"));
    assert_eq!(code.binding, first.binding);
    assert_ne!(code.binding, second.binding);
    for callback in [
        "https://antigravity.google/oauth-callback?code=fixture-code&state=second-state",
        "https://antigravity.google/oauth-callback?code=fixture-code",
        "https://antigravity.google/oauth-callback?code=fixture-code&state=first-state&state=first-state",
        "https://antigravity.google/oauth-callback?code=fixture-code%0Anext&state=first-state",
        "https://antigravity.google/oauth-callback?code=fixture-code&state=first-state&error=denied",
        "https://antigravity.google.evil.test/oauth-callback?code=fixture-code&state=first-state",
        "https://antigravity.google/elsewhere?code=fixture-code&state=first-state",
    ] {
        assert!(first.callback(callback).is_err());
    }
    for url in [
        request("first-state") + "&client_id=other.apps.googleusercontent.com",
        request("first-state").replace("S256", "plain"),
        request("first-state").replace("antigravity.google", "attacker.test"),
        request("first-state").replace("auth/aicode", "auth/gmail.readonly"),
        request("first-state").replace("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "short"),
    ] {
        assert!(Authorization::admit(&url).is_err());
    }
}

#[test]
fn expired_native_attempt_starts_no_child_and_creates_no_profile() {
    let root = aterm_tempfile::tempdir().unwrap();
    let profile = root.path().join("not-created");
    let result = run(
        Path::new("/usr/bin/true"),
        &profile,
        WakeDeadline::after(Duration::ZERO),
        |_, _| panic!("expired attempt invoked approval"),
    );
    assert!(matches!(result, Err(Error::Deadline)));
    assert!(!profile.exists());
}

#[test]
fn explicit_google_path_needs_credentials_only_after_a_native_handoff() {
    let state = aterm_tempfile::tempdir().unwrap();
    let home = aterm_tempfile::tempdir().unwrap();
    let registry = "[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n";
    aterm_uds::private_file::write_private(
        state.path(),
        "google-identities.toml",
        registry.as_bytes(),
    )
    .unwrap();
    let result = run_google(
        Path::new("/not-a-native-program"),
        state.path(),
        home.path(),
        "one",
        WakeDeadline::after(Duration::from_secs(30)),
    );
    assert!(matches!(
        result,
        Err(GoogleLoginError::Native(Error::Profile))
    ));
    assert!(!state.path().join("agy-logins").exists());
    assert!(!state.path().join("google-identities").exists());
    let result = run_google(
        Path::new("/not-a-native-program"),
        state.path(),
        home.path(),
        "other",
        WakeDeadline::after(Duration::from_secs(30)),
    );
    assert!(matches!(result, Err(GoogleLoginError::Configuration)));
}

#[test]
fn staging_claim_precedes_seed_and_refuses_existing_grants() {
    let root = aterm_tempfile::tempdir().unwrap();
    let previous = root.path().join("previous");
    let staging = root.path().join("staging");
    let suffix = ".gemini/antigravity-cli/antigravity-oauth-token";
    let source = previous.join(suffix);
    aterm_secrets::fsx::ensure_private_dir(source.parent().unwrap()).unwrap();
    aterm_secrets::fsx::write_atomic(&source, b"original", 0o600).unwrap();
    let claim = stage_profile(&staging, Some(&previous)).unwrap();
    assert!(super::super::browser_window::ProfileLock::claim(&staging).is_err());
    aterm_secrets::fsx::write_atomic(&source, b"new-live-grant", 0o600).unwrap();
    assert!(matches!(
        stage_profile(&staging, Some(&previous)),
        Err(Error::Profile)
    ));
    assert_eq!(std::fs::read(staging.join(suffix)).unwrap(), b"original");
    drop(claim);
    assert!(matches!(
        stage_profile(&staging, Some(&previous)),
        Err(Error::Profile)
    ));
    assert_eq!(std::fs::read(&source).unwrap(), b"new-live-grant");
    assert_eq!(std::fs::read(staging.join(suffix)).unwrap(), b"original");
}

#[test]
fn staged_google_login_binds_the_row_before_touching_its_profile() {
    let state = aterm_tempfile::tempdir().unwrap();
    let home = aterm_tempfile::tempdir().unwrap();
    aterm_uds::private_file::write_private(
        state.path(),
        "google-identities.toml",
        b"[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n",
    )
    .unwrap();
    let staging = state.path().join("staged");
    let previous = state.path().join("previous");
    let attempt = |email: &str, profile: &Path| {
        run_google_staged(
            Path::new("/not-a-native-program"),
            state.path(),
            home.path(),
            "one",
            StagedGoogleLogin {
                profile,
                previous: Some(&previous),
                expected_email: email,
                recovery: None,
            },
            WakeDeadline::after(Duration::from_secs(30)),
        )
    };
    assert!(matches!(
        attempt("other@example.com", &staging),
        Err(GoogleLoginError::Configuration)
    ));
    assert!(!staging.exists());
    assert!(matches!(
        attempt("person@example.com", Path::new("relative")),
        Err(GoogleLoginError::Configuration)
    ));
    let registry = registry(state.path()).unwrap();
    let identity = registry.resolve("agy", "one").unwrap().0;
    let active = native_profile(state.path(), "one", identity);
    assert!(matches!(
        attempt("person@example.com", &active),
        Err(GoogleLoginError::Configuration)
    ));
    assert!(!active.exists());
    let token = previous.join(".gemini/antigravity-cli/antigravity-oauth-token");
    aterm_secrets::fsx::ensure_private_dir(token.parent().unwrap()).unwrap();
    aterm_secrets::fsx::write_atomic(&token, b"synthetic-native-grant", 0o600).unwrap();
    assert!(matches!(
        attempt("PERSON@example.com", &staging),
        Err(GoogleLoginError::Native(Error::Profile))
    ));
    assert!(!active.exists());
    assert_eq!(std::fs::read(&token).unwrap(), b"synthetic-native-grant");
    assert_eq!(
        std::fs::read(staging.join(".gemini/antigravity-cli/antigravity-oauth-token")).unwrap(),
        b"synthetic-native-grant"
    );
}

#[cfg(target_vendor = "apple")]
#[test]
fn xnu_zombie_only_group_is_a_completed_cleanup() {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;

    struct Forked(i32);
    impl Drop for Forked {
        fn drop(&mut self) {
            if self.0 <= 1 {
                return;
            }
            let mut status = 0;
            // SAFETY: this test's child leads its own group and remains unreaped here.
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
                libc::waitpid(self.0, &raw mut status, 0);
            }
        }
    }

    let (mut proof_r, proof_w) = aterm_uds::pipe().unwrap();
    let (proof_r_fd, proof_w_fd) = (proof_r.as_raw_fd(), proof_w.as_raw_fd());
    // The proof's write end is the child's: unmarked for this fork alone.
    let handing = aterm_uds::spawn_lock::handing(&[proof_w_fd]);
    // SAFETY: the child calls only async-signal-safe functions (`close`, `setpgid`, `write`,
    // `_exit`) on descriptors prepared before the fork, then exits without unwinding.
    let pid = unsafe { libc::fork() };
    if pid != 0 {
        // The child's copy of the hold is never released: it `_exit`s.
        drop(handing);
    }
    assert!(pid >= 0, "fork: {}", std::io::Error::last_os_error());
    if pid == 0 {
        // SAFETY: the forked child follows the contract above.
        unsafe {
            libc::close(proof_r_fd);
            let ok = libc::setpgid(0, 0) == 0;
            let byte = u8::from(ok);
            libc::write(proof_w_fd, (&raw const byte).cast(), 1);
            libc::close(proof_w_fd);
            libc::_exit(i32::from(!ok));
        }
    }
    assert!(pid > 1, "a forked child has a positive process ID");
    let mut child = Forked(pid);
    drop(proof_w);
    let mut proof = [0u8; 1];
    proof_r.read_exact(&mut proof).unwrap();
    assert_eq!(proof, [1], "the child made itself its group leader");
    drop(proof_r);

    let until = Instant::now() + Duration::from_secs(60);
    loop {
        // SAFETY: signal zero observes this child's positive, uniquely held group.
        let found = unsafe { libc::kill(-pid, 0) };
        let error = std::io::Error::last_os_error();
        if found < 0 && error.raw_os_error() == Some(libc::EPERM) {
            break;
        }
        assert!(
            Instant::now() < until,
            "the child did not reach XNU's zombie-only group state: {error}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    finish_exited_group(pid).expect("a complete zombie-only group is already ended");

    let mut status = 0;
    // SAFETY: this test owns and reaps this child exactly once.
    assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
    child.0 = 0;
    assert!(libc::WIFEXITED(status));
    assert_eq!(libc::WEXITSTATUS(status), 0);
}

#[test]
fn rejected_staging_reset_preserves_refusals_and_concurrent_changes() {
    const SAVED: &[u8] = br#"{"auth_method":"consumer","token":{"access_token":"synthetic-only"}}"#;
    for scenario in 0..8 {
        let profile = aterm_tempfile::tempdir().unwrap();
        let path = profile
            .path()
            .join(".gemini/antigravity-cli/antigravity-oauth-token");
        aterm_secrets::fsx::ensure_private_dir(path.parent().unwrap()).unwrap();
        let saved = if scenario == 7 {
            br#"{"auth_method":"consumer","metadata":"keep","token":{"access_token":"synthetic-only","refresh_token":"synthetic-refresh","expiry":"2100-01-01T00:00:00Z"}}"#.as_slice()
        } else {
            SAVED
        };
        aterm_secrets::fsx::write_atomic(&path, saved, 0o600).unwrap();
        let current = std::cell::Cell::new(true);
        let claim = super::super::browser_window::ProfileLock::claim(profile.path()).unwrap();
        let deadline = if scenario == 6 {
            WakeDeadline::after(Duration::ZERO)
        } else {
            WakeDeadline::after(Duration::from_secs(3600))
        };
        let result = prepare_rejected_staging(
            profile.path(),
            &StagedRecovery {
                expected_email: "person@example.com",
                current: &|| current.get(),
            },
            deadline,
            |token, wait| {
                assert_eq!(token, "synthetic-only");
                assert!(
                    !wait.is_zero() && wait <= crate::harness::google_profile::REQUEST_HANG,
                    "the account check is capped, not given the hour: {wait:?}"
                );
                assert!(super::super::browser_window::ProfileLock::claim(profile.path()).is_err());
                match scenario {
                    0 => return Ok(()),
                    1 => {
                        return Err(super::super::google_profile::Error::Refused(
                            "transport failed",
                        ));
                    }
                    2 => {
                        return Err(super::super::google_profile::Error::Refused(
                            "identity mismatch",
                        ));
                    }
                    4 => current.set(false),
                    5 => aterm_secrets::fsx::write_atomic(&path, b"replaced", 0o600).unwrap(),
                    _ => {}
                }
                Err(super::super::google_profile::Error::CredentialRejected)
            },
        );
        match scenario {
            0..=2 => assert_eq!(result, Ok(false)),
            3 | 7 => assert_eq!(result, Ok(true)),
            4 => assert_eq!(result, Err(Error::Authorization)),
            5 => assert_eq!(result, Err(Error::Profile)),
            6 => assert_eq!(result, Err(Error::Deadline)),
            _ => unreachable!(),
        }
        if scenario == 3 {
            assert!(!path.exists());
        } else if scenario == 7 {
            let mut expected: aterm_json::Value = aterm_json::from_slice(saved).unwrap();
            expected
                .as_object_mut()
                .unwrap()
                .get_mut("token")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("expiry".into(), "2000-01-01T00:00:00Z".into());
            let actual: aterm_json::Value =
                aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(actual, expected);
        } else {
            assert_eq!(
                std::fs::read(&path).unwrap(),
                if scenario == 5 {
                    b"replaced".as_slice()
                } else {
                    SAVED
                }
            );
        }
        drop(claim);
    }
}

#[test]
#[ignore = "contacts the installed native agy anonymously; ATERM_TEST_AGY names its pinned binary"]
fn native_expired_and_malformed_grants_reach_a_fresh_google_handoff() {
    let program = std::env::var_os("ATERM_TEST_AGY")
        .map(PathBuf::from)
        .expect("ATERM_TEST_AGY is required");
    let mut failed = Vec::new();
    for (case, seed) in [
        ("empty", None),
        ("expired", Some(br#"{"auth_method":"consumer","id_token_email":"synthetic@example.invalid","token":{"access_token":"aterm-synthetic-access","token_type":"Bearer","refresh_token":"aterm-synthetic-invalid-refresh","expiry":"2000-01-01T00:00:00Z"}}"#.as_slice())),
        ("rejected-before-expiry", Some(br#"{"auth_method":"consumer","id_token_email":"synthetic@example.invalid","token":{"access_token":"aterm-synthetic-access","token_type":"Bearer","refresh_token":"aterm-synthetic-invalid-refresh","expiry":"2100-01-01T00:00:00Z"}}"#.as_slice())),
        ("malformed", Some(b"invalid synthetic native grant".as_slice())),
    ] {
        let profile = aterm_tempfile::tempdir().unwrap();
        if let Some(seed) = seed {
            let path = profile.path().join(".gemini/antigravity-cli/antigravity-oauth-token");
            aterm_secrets::fsx::ensure_private_dir(path.parent().unwrap()).unwrap();
            aterm_secrets::fsx::write_atomic(&path, seed, 0o600).unwrap();
        }
        let staging = aterm_tempfile::tempdir().unwrap();
        let claim = stage_profile(staging.path(), Some(profile.path())).unwrap();
        let original = std::fs::read(profile.path().join(".gemini/antigravity-cli/antigravity-oauth-token")).ok();
        let mut observed = false;
        let result = run_claimed(&program, staging.path(), Some(claim), Some(StagedRecovery { expected_email: "synthetic@example.invalid", current: &|| true }), WakeDeadline::after(Duration::from_secs(60)), |_, _| {
            observed = true;
            Err(Error::Callback)
        });
        println!("native recovery {case}: handoff={observed} failure={:?}", result.as_ref().err());
        if !observed || !matches!(result, Err(Error::Callback)) {
            failed.push(case);
        }
        assert_eq!(std::fs::read(profile.path().join(".gemini/antigravity-cli/antigravity-oauth-token")).ok(), original);
        let _claim = super::super::browser_window::ProfileLock::claim(staging.path()).unwrap();
    }
    assert!(
        failed.is_empty(),
        "native recovery omitted Google handoff: {failed:?}"
    );
}

#[test]
#[ignore = "contacts the installed native agy anonymously; ATERM_TEST_AGY names its pinned binary"]
fn native_headless_handoff_is_admitted_and_cancellation_releases_its_profile() {
    let program = std::env::var_os("ATERM_TEST_AGY")
        .map(PathBuf::from)
        .expect("ATERM_TEST_AGY is required");
    let profile = aterm_tempfile::tempdir().unwrap();
    let mut observed = false;
    let result = run(
        &program,
        profile.path(),
        WakeDeadline::after(Duration::from_secs(30)),
        |request, _| {
            assert!(
                request
                    .url()
                    .starts_with("https://accounts.google.com/o/oauth2/auth?")
            );
            observed = true;
            Err(Error::Callback)
        },
    );
    assert!(observed, "native handoff was not observed");
    assert!(matches!(result, Err(Error::Callback)));
    let _claim = super::super::browser_window::ProfileLock::claim(profile.path()).unwrap();
}

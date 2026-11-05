// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::json;
use std::cell::Cell;

#[test]
fn native_launch_refuses_unknown_local_policy_before_credential_selection() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let config = temp.path().join("aterm.toml");
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    with_test_launcher(
        move |_, _| {
            observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Some(PathBuf::from("/synthetic/aterm")))
        },
        || {
            let run = || {
                launch(
                    Provider::Google,
                    Path::new("/synthetic/agy"),
                    &[],
                    &[],
                    Some(&config),
                )
            };
            assert!(run().unwrap().is_some());
            aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
            assert!(run().unwrap().is_some());
            assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
            let mut refused = Vec::new();
            for unreadable in [false, true] {
                if unreadable {
                    std::fs::remove_file(&config).unwrap();
                    std::fs::create_dir(&config).unwrap();
                } else {
                    std::fs::write(&config, "[harness\naccounts = 'off'\n").unwrap();
                }
                refused.push(run().is_err());
            }
            assert_eq!(refused, vec![true, true]);
            assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
            for (args, env) in [
                (vec!["--gemini_dir=/person/profile".into()], vec![]),
                (vec![], vec![("GEMINI_API_KEY", "person-selected")]),
            ] {
                assert!(
                    launch(
                        Provider::Google,
                        Path::new("/synthetic/agy"),
                        &args,
                        &env,
                        Some(&config)
                    )
                    .unwrap()
                    .is_none()
                );
            }
            assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
        },
    );
}

fn input(provider: Provider, key: &str) -> Inputs {
    Inputs::decode(
        &aterm_json::to_vec(&json!({
            "schema":1,"kind":"api-key","provider":provider.name(),"key":key
        }))
        .unwrap(),
        provider,
    )
    .unwrap()
}

#[test]
fn native_raw_key_encoding_is_closed_and_provider_selected() {
    let bytes = Inputs::encode_key(Provider::Google, b"synthetic-key").unwrap();
    assert!(Inputs::decode(bytes.expose(), Provider::Google).is_ok());
    assert!(Inputs::decode(bytes.expose(), Provider::Meta).is_err());
    assert!(Inputs::encode_key(Provider::Meta, b"bad\nheader").is_err());
    assert!(!format!("{bytes:?}").contains("synthetic-key"));
}

#[test]
fn native_default_route_parser_preserves_explicit_native_choices() {
    for (provider, args) in [
        (Provider::Google, vec!["-gemini_dir=/local"]),
        (Provider::Google, vec!["-app_data_dir", "local"]),
        (Provider::Google, vec!["--settings={}"]),
        (Provider::Google, vec!["--future-route", "x"]),
        (Provider::Meta, vec!["--preset", "local"]),
        (Provider::Meta, vec!["-cprovider=other"]),
        (Provider::Meta, vec!["--base-url=https://other.invalid"]),
        (Provider::Meta, vec!["resume", "saved"]),
    ] {
        assert!(!default_route(
            provider,
            &args.iter().map(|word| (*word).into()).collect::<Vec<_>>()
        ));
    }
    for (provider, args) in [
        (
            Provider::Google,
            vec![
                "--model",
                "example",
                "--prompt-interactive",
                "--settings=is-prompt-text",
            ],
        ),
        (
            Provider::Meta,
            vec![
                "exec",
                "--disable-write",
                "--prompt-file",
                "/private/prompt",
            ],
        ),
    ] {
        assert!(default_route(
            provider,
            &args.iter().map(|word| (*word).into()).collect::<Vec<_>>()
        ));
    }
    assert!(explicit_env(Provider::Google, "AGY_GATEWAY_URL"));
    assert!(explicit_env(Provider::Meta, "MUSE_CUSTOM_HEADERS"));
    assert!(explicit_env(Provider::Meta, "TBH_CREDENTIAL_BACKEND"));
}

fn body(provider: Provider) -> Vec<u8> {
    aterm_json::to_vec(&match provider {
        Provider::Google => json!({"models":[{"name":"models/example"}]}),
        Provider::Meta => json!({"object":"list","data":[{"object":"model","id":"example"}]}),
    })
    .unwrap()
}

struct Server {
    statuses: Vec<u16>,
    saw: Vec<bool>,
}

impl Probe for Server {
    fn get(&mut self, provider: Provider, key: Option<&[u8]>, _: Instant) -> Result<Reply, String> {
        self.saw.push(key.is_some());
        let status = self.statuses.remove(0);
        Ok(Reply {
            status,
            body: Secret::from_vec(body(provider)),
        })
    }
}

#[test]
fn native_keys_reject_other_providers_session_formats_and_header_injection() {
    for value in [
        json!({"schema":1,"kind":"api-key","provider":"meta","key":"test"}),
        json!({"schema":1,"kind":"oauth","provider":"google","key":"test"}),
        json!({"schema":1,"kind":"api-key","provider":"google","key":"test","endpoint":"https://elsewhere.invalid"}),
        json!({"schema":1,"kind":"api-key","provider":"google","key":"line\r\nheader"}),
        json!({"schema":1,"kind":"api-key","provider":"google","key":""}),
    ] {
        assert!(Inputs::decode(&aterm_json::to_vec(&value).unwrap(), Provider::Google).is_err());
    }
}

#[test]
fn native_keys_require_key_dependent_access_not_a_public_catalog() {
    for provider in [Provider::Google, Provider::Meta] {
        for (statuses, succeeds, calls) in [
            (vec![200], false, vec![false]),
            (vec![404], false, vec![false]),
            (vec![401, 401], false, vec![false, true]),
            (vec![403, 200], true, vec![false, true]),
        ] {
            let mut server = Server {
                statuses,
                saw: vec![],
            };
            let result = Access::check(
                input(provider, "test-key"),
                &mut server,
                Instant::now() + CHECK_BUDGET,
                || Ok(()),
            );
            assert_eq!(result.is_ok(), succeeds);
            assert_eq!(server.saw, calls);
        }
    }
}

#[test]
fn native_keys_recheck_authority_before_secret_delivery_and_after_provider_reply() {
    for revoked_at in [1, 2, 3] {
        let count = Cell::new(0);
        let mut server = Server {
            statuses: vec![401, 200],
            saw: vec![],
        };
        assert!(
            Access::check(
                input(Provider::Meta, "test-key"),
                &mut server,
                Instant::now() + CHECK_BUDGET,
                || {
                    count.set(count.get() + 1);
                    if count.get() == revoked_at {
                        Err("revoked".into())
                    } else {
                        Ok(())
                    }
                }
            )
            .is_err()
        );
        assert_eq!(server.saw.len(), revoked_at - 1);
    }
}

#[test]
fn native_keys_exact_input_and_independent_permission_fence_child_environment() {
    let provider = Provider::Google;
    let mut server = Server {
        statuses: vec![401, 200],
        saw: vec![],
    };
    let access = Access::check(
        input(provider, "test-key"),
        &mut server,
        Instant::now() + CHECK_BUDGET,
        || Ok(()),
    )
    .unwrap();
    let mut command = aterm_uds::spawnfd::command("/not-executed");
    command.env("UNRELATED_PROFILE", "preserved");
    assert!(
        access
            .bind_child(&input(provider, "changed"), &mut command, || Ok(()))
            .is_err()
    );
    assert!(
        access
            .bind_child(&input(provider, "test-key"), &mut command, || Err(
                "not assigned".into()
            ))
            .is_err()
    );
    assert!(
        !command
            .get_envs()
            .any(|(name, _)| name == provider.key_env())
    );
    access
        .bind_child(&input(provider, "test-key"), &mut command, || Ok(()))
        .unwrap();
    assert_eq!(
        command
            .get_envs()
            .find(|(name, _)| *name == provider.key_env())
            .unwrap()
            .1
            .unwrap(),
        "test-key"
    );
    assert_eq!(
        command
            .get_envs()
            .find(|(name, _)| *name == "UNRELATED_PROFILE")
            .unwrap()
            .1
            .unwrap(),
        "preserved"
    );
}

#[test]
fn native_keys_expired_check_starts_no_provider_request() {
    let mut server = Server {
        statuses: vec![],
        saw: vec![],
    };
    assert!(
        Access::check(
            input(Provider::Google, "test-key"),
            &mut server,
            Instant::now(),
            || Ok(())
        )
        .is_err()
    );
    assert!(server.saw.is_empty());
}

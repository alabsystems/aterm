// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::json;

#[cfg(target_os = "macos")]
mod native_reader {
    use super::*;
    use aterm_agent::harness::{align::Captured, cli::AccountsHost};
    use std::{
        os::unix::fs::PermissionsExt as _,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    struct Inert;
    impl AccountsHost for Inert {
        fn stdin_is_terminal(&self) -> bool {
            false
        }
        fn auth_status(&self, _: &str) -> Result<Captured, String> {
            panic!("observation must not run an ambient status command")
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            panic!("observation must not run a login")
        }
        fn now(&self) -> i64 {
            1_800_000_000
        }
        fn status_in_home(&self, _: &str) -> Result<Captured, String> {
            panic!("credential refusal must precede status or provider effects")
        }
    }

    struct Native(Inert);
    impl AccountsHost for Native {
        fn native_credential_setup(&self) -> bool {
            true
        }
        fn stdin_is_terminal(&self) -> bool {
            self.0.stdin_is_terminal()
        }
        fn auth_status(&self, login: &str) -> Result<Captured, String> {
            self.0.auth_status(login)
        }
        fn unaskable(&self) -> Option<String> {
            self.0.unaskable()
        }
        fn login(&self, dir: Option<&str>) -> Result<i32, String> {
            self.0.login(dir)
        }
        fn now(&self) -> i64 {
            self.0.now()
        }
        fn status_in_home(&self, home: &str) -> Result<Captured, String> {
            self.0.status_in_home(home)
        }
    }

    fn environment(home: &Path) -> cli::Env {
        cli::Env {
            state: home.join("state"),
            aterm_state: None,
            cwd: home.to_owned(),
            home: Some(home.to_owned()),
            claude_dir: None,
            claude_config_unusable: None,
            claude_profile: None,
            claude_custom_oauth: false,
            claude_session: None,
            claude_pid: None,
            codex_thread: None,
            codex_home: None,
            xdg_config_home: None,
            codex_login: None,
            agy_conversation: None,
            now: 1_800_000_000,
            sid: "native-reader-fixture".into(),
            sock: None,
            config: None,
            claude_tmp: None,
            claude_running: None,
            tz_s: 0,
        }
    }

    fn fixture() -> (aterm_tempfile::TempDir, cli::Env, accounts::Row) {
        let stage = aterm_tempfile::tempdir().unwrap();
        std::fs::set_permissions(stage.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let profile = stage.path().join(".claude.json");
        std::fs::write(&profile, b"{}").unwrap();
        std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            accounts::login_made(stage.path()),
            "observation must reach credential discovery"
        );
        assert!(!stage.path().join(".credentials.json").exists());
        let env = environment(stage.path());
        let row = accounts::Row {
            label: "shared-native-reader".into(),
            email: Some("owner@example.test".into()),
            ..Default::default()
        };
        (stage, env, row)
    }

    fn refusing_reader(
        stage: &Path,
    ) -> (Arc<cli::shared_provider::KeychainReader>, Arc<AtomicUsize>) {
        let expected = stage.to_owned();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let reader: Arc<cli::shared_provider::KeychainReader> = Arc::new(move |path, _| {
            assert_eq!(path, expected);
            counted.fetch_add(1, Ordering::SeqCst);
            Err("injected native reader denial".into())
        });
        (reader, calls)
    }

    #[test]
    fn an_inert_host_keeps_observation_on_the_file_credential_backend() {
        let (stage, env, row) = fixture();
        let store = env.accounts_store();
        let host = Inert;
        assert!(
            !host.native_credential_setup(),
            "the default host must stay inert"
        );
        let context = Context {
            publication: None,
            admission: None,
            env: &env,
            store: &store,
            host: &host,
        };
        let absent = ClaudeOauth
            .observe(&context, &row, stage.path())
            .err()
            .expect("missing credentials must refuse");
        let (reader, calls) = refusing_reader(stage.path());
        let observed = claude_observe_with_reader(&context, &row, stage.path(), reader);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "an inert host must not invoke a native reader"
        );
        assert_eq!(observed.err(), Some(absent));
    }

    #[test]
    fn an_enabled_native_reader_denial_cannot_fall_back_to_file_credentials() {
        let (stage, env, row) = fixture();
        let credential = stage.path().join(".credentials.json");
        std::fs::write(
            &credential,
            br#"{"claudeAiOauth":{"accessToken":"synthetic-only"}}"#,
        )
        .unwrap();
        std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            credential.is_file(),
            "file credentials must be present beside the denial"
        );
        let store = env.accounts_store();
        let host = Native(Inert);
        assert!(host.native_credential_setup());
        let context = Context {
            publication: None,
            admission: None,
            env: &env,
            store: &store,
            host: &host,
        };
        let (reader, calls) = refusing_reader(stage.path());
        let observed = claude_observe_with_reader(&context, &row, stage.path(), reader);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            observed.err(),
            Some(ObservationError::Unavailable(
                "injected native reader denial".into()
            ))
        );
    }
}

#[test]
fn only_registered_device_local_provider_routes_are_admitted() {
    for method in ["oauth", "browser", "device"] {
        assert_eq!(
            Kind::of(&json!({"provider":"openai","method":method}), false).unwrap(),
            Kind::Codex
        );
    }
    assert_eq!(
        Kind::of(&json!({"provider":"anthropic","method":"oauth"}), false).unwrap(),
        Kind::Claude
    );
    for (provider, method) in [
        ("aterm-verification", "api-key"),
        ("openai", "api-key"),
        ("anthropic", "device"),
    ] {
        assert!(Kind::of(&json!({"provider":provider,"method":method}), false).is_err());
    }
    assert!(Kind::of(&json!({"provider":"openai","method":"oauth"}), true).is_err());
    assert!(Kind::of(&json!({"provider":"anthropic","method":"oauth"}), true).is_err());
    assert_eq!(
        Kind::of(
            &json!({"provider":"anthropic","method":"oauth",
            "details":{"login-input-format":{"kind":"requirement","value":"mailbox-v1"}}}),
            true
        )
        .unwrap(),
        Kind::Claude,
    );
    assert_eq!(
        Kind::of(
            &json!({"provider":"openai","method":"oauth",
        "details":{"login-input-format":{"kind":"requirement","value":"mailbox-v1"}}}),
            true
        )
        .unwrap(),
        Kind::Codex
    );
    assert!(Kind::of(&json!({"provider":"kimi","method":"device"}), false).is_err());
    assert_eq!(
        Kind::of(
            &json!({"provider":"kimi","method":"device","realm":"global"}),
            false
        )
        .unwrap(),
        Kind::Kimi
    );
    assert!(
        Kind::of(
            &json!({"provider":"kimi","method":"device","realm":"global",
        "details":{"region":{"kind":"requirement","value":"mainland-cn"}}}),
            false
        )
        .is_err()
    );
}

#[test]
fn codex_proposal_is_discovered_only_after_current_root_preparation() {
    use aterm_db::{accounts::proposals::Proposals, database::Change};
    use aterm_secrets::commons::bootstrap::Bootstrap;
    let temp = aterm_tempfile::tempdir().unwrap();
    let home = temp.path().join("machine");
    let paths = Paths::for_home(&home);
    client::create(
        &paths,
        temp.path().join("vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    Proposals::at(paths.clone())
        .write(
            [1; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "codex-one".into(),
                value: Some(
                    json!({"provider":"openai","method":"oauth","email":"approved@example.test"}),
                ),
            }],
        )
        .unwrap();
    Activations::at(paths.clone()).initialize().unwrap();
    assert!(
        super::super::automatic_candidates(&home)
            .unwrap()
            .is_empty()
    );
    let admin = client::admin_open(&paths).unwrap();
    let actor = Bootstrap::required(&paths)
        .unwrap()
        .request()
        .unwrap()
        .binding()
        .actor();
    let db = Activations::at(paths.clone());
    let request = db
        .configuration(&admin, [7; 32], "codex-one", actor, Vec::new())
        .unwrap();
    let (record, _) = db.issue(&admin, [2; 16], request).unwrap();
    let candidates = super::super::automatic_candidates(&home).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].cli, accounts::Cli::Codex);
    assert_eq!(
        candidates[0].row.email.as_deref(),
        Some("approved@example.test")
    );
    let stopped = db.stop_request([7; 32], vec![record.id()]).unwrap();
    db.issue(&admin, [3; 16], stopped).unwrap();
    assert!(
        super::super::automatic_candidates(&home)
            .unwrap()
            .is_empty()
    );
}

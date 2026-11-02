// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const EMAIL: &str = "kimi-fixture@example.test";
const GLOBAL_FILE: &str = "credentials/kimi-code-env-0e4f99c69cc27850.json";

fn home() -> aterm_tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().expect("scratch home");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
        .expect("private home");
    std::fs::create_dir(dir.path().join("credentials")).expect("credentials directory");
    std::fs::set_permissions(
        dir.path().join("credentials"),
        std::fs::Permissions::from_mode(0o700),
    )
    .expect("private credentials");
    std::fs::write(dir.path().join(GLOBAL_FILE),
        r#"{"access_token":"fixture-access","refresh_token":"fixture-refresh","token_type":"Bearer","expires_at":2000}"#).expect("token");
    std::fs::set_permissions(
        dir.path().join(GLOBAL_FILE),
        std::fs::Permissions::from_mode(0o600),
    )
    .expect("private token");
    std::fs::write(dir.path().join("config.toml"), format!(
        "default_model = \"model\"\n[providers.\"{PROVIDER}\"]\ntype = \"kimi\"\nbase_url = \"{}\"\napi_key = \"\"\n[providers.\"{PROVIDER}\".oauth]\nstorage = \"file\"\nkey = \"oauth/kimi-code-env-0e4f99c69cc27850\"\noauth_host = \"https://auth.kimi.ai\"\n[models.model]\nprovider = \"{PROVIDER}\"\n", base(Region::Global))).expect("config");
    dir
}

fn body() -> Vec<u8> {
    format!(r#"{{"user_id":"whole-user-id-not-an-abbreviation","email":"{EMAIL}"}}"#).into_bytes()
}

#[test]
fn exact_accepted_credential_and_config_are_fenced_before_publication() {
    assert_eq!(
        oauth_key(Region::Global),
        "oauth/kimi-code-env-0e4f99c69cc27850"
    );
    assert_eq!(oauth_key(Region::MainlandCn), "oauth/kimi-code");
    let dir = home();
    let proof = verify_with(dir.path(), Region::Global, EMAIL, 1000, |url, token| {
        assert_eq!(url, "https://api.kimi.ai/coding/v1/me");
        assert_eq!(token, "fixture-access");
        Ok((200, body()))
    })
    .expect("accepted")
    .expect("proof");
    assert_eq!(proof.identity(), EMAIL);
    assert_eq!(
        proof.principal(),
        KimiOauth::principal(Region::Global, "whole-user-id-not-an-abbreviation")
            .expect("principal")
    );
    assert_ne!(
        proof.principal(),
        KimiOauth::principal(Region::MainlandCn, "whole-user-id-not-an-abbreviation")
            .expect("principal")
    );
    proof.check_current().expect("unchanged");
    std::fs::write(dir.path().join(GLOBAL_FILE), "changed").expect("token edit");
    assert!(proof.check_current().is_err());
}

#[test]
fn provider_identity_and_http_acceptance_cannot_be_replaced_by_file_presence() {
    let dir = home();
    for (status, body) in [
        (401, body()),
        (302, body()),
        (200, br#"{"email":"kimi-fixture@example.test"}"#.to_vec()),
        (
            200,
            br#"{"user_id":"other","email":"somebody-else@example.test"}"#.to_vec(),
        ),
    ] {
        assert!(
            verify_with(dir.path(), Region::Global, EMAIL, 1000, |_, _| Ok((
                status, body
            )))
            .is_err()
        );
    }
    assert!(
        verify_with(dir.path(), Region::Global, EMAIL, 2000, |_, _| panic!(
            "expired token must not leave the machine"
        ))
        .is_err()
    );
    assert!(
        verify_with(dir.path(), Region::MainlandCn, EMAIL, 1000, |_, _| panic!(
            "wrong-region config must not leave the machine"
        ))
        .is_err()
    );
}

#[test]
fn replaced_config_credential_link_and_empty_configs_fail_closed() {
    let dir = home();
    let config = std::fs::read(dir.path().join("config.toml")).expect("original config");
    let proof = verify_with(dir.path(), Region::Global, EMAIL, 1000, |_, _| {
        Ok((200, body()))
    })
    .expect("accepted")
    .expect("proof");
    std::fs::write(dir.path().join("config.toml"), "").expect("change config");
    assert!(proof.check_current().is_err());
    assert!(
        verify_with(dir.path(), Region::Global, EMAIL, 1000, |_, _| panic!(
            "invalid config must not leave the machine"
        ))
        .is_err()
    );
    std::fs::write(dir.path().join("config.toml"), config).expect("restore valid config");
    std::fs::remove_file(dir.path().join(GLOBAL_FILE)).expect("remove fixture");
    std::os::unix::fs::symlink("../config.toml", dir.path().join(GLOBAL_FILE))
        .expect("link fixture");
    assert!(
        verify_with(dir.path(), Region::Global, EMAIL, 1000, |_, _| panic!(
            "linked credential must not leave the machine"
        ))
        .is_err()
    );
}

#[test]
fn kimi_adapter_uses_intrinsic_device_route_and_requires_explicit_region() {
    struct Host;
    impl AccountsHost for Host {
        fn now(&self) -> i64 {
            1000
        }
        fn stdin_is_terminal(&self) -> bool {
            false
        }
        fn auth_status(&self, _: &str) -> Result<Captured, String> {
            panic!("no Claude query")
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            panic!("no ordinary login")
        }
        fn vendor_sign_in(&self, req: &login_drive::Request<'_>) -> login_drive::Drove {
            assert_eq!(req.cli, Cli::Kimi);
            assert_eq!(req.row.vendor.region, Some(Region::Global));
            assert!(req.hosts.contains(&"auth.kimi.ai"));
            login_drive::Drove {
                outcome: login_drive::Outcome::NotRun("fixture no start".into()),
                host: None,
            }
        }
    }
    let temp = home();
    let env = Env {
        state: temp.path().join("state"),
        aterm_state: None,
        cwd: temp.path().to_owned(),
        home: Some(temp.path().to_owned()),
        claude_dir: None,
        claude_config_unusable: None,
        claude_profile: None,
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        codex_home: None,
        codex_login: None,
        agy_conversation: None,
        now: 1000,
        sid: String::new(),
        sock: None,
        config: None,
        claude_tmp: None,
        claude_running: None,
        tz_s: 0,
    };
    let store = accounts::Store::at_for(&env.state, Cli::Kimi, temp.path());
    let context = Context {
        env: &env,
        store: &store,
        host: &Host,
    };
    let mut row = accounts::Row {
        email: Some(EMAIL.into()),
        vendor: accounts::Vendor {
            region: Some(Region::Global),
            flow: None,
        },
        ..Default::default()
    };
    assert_eq!(row.flow(Cli::Kimi), None);
    assert!(matches!(
        KimiOauth.execute_attempt(&context, &row, temp.path(), &RenewBrowser::Default),
        Execution::NotStarted(_)
    ));
    row.vendor.region = None;
    assert!(request(&context, &row, temp.path()).is_err());
    row.vendor.region = Some(Region::Global);
    row.vendor.flow = Some(Flow::Browser);
    assert!(request(&context, &row, temp.path()).is_err());
}

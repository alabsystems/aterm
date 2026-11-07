// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Canonical admission fixture. Synthetic provider bytes exercise the real
//! encrypted store and generation writer; this does not qualify OAuth or the
//! prospective launch/publication barrier.

use super::super::{account_work, accounts, codex_session_slot};
use super::*;
use std::cell::RefCell;

fn document(now: i64) -> Vec<u8> {
    let claims = aterm_json::json!({
        "sub":"fixture-user", "email":"fixture@example.test", "exp":now+3600,
        "https://api.openai.com/auth":{
            "chatgpt_account_id":"fixture-account", "chatgpt_user_id":"fixture-user"
        }
    });
    let payload = aterm_codec::base64::encode(&aterm_json::to_vec(&claims).unwrap())
        .unwrap()
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_");
    let jwt = format!("e30.{payload}.c2ln");
    aterm_json::to_vec(&aterm_json::json!({
        "auth_mode":"chatgpt", "last_refresh":now,
        "tokens":{"account_id":"fixture-account","id_token":jwt,"access_token":jwt,
            "refresh_token":"fixture-refresh-only", "login_owner_id":"fixture-code-owner"}
    }))
    .unwrap()
}

pub(super) struct Fixture {
    _world: aterm_tempfile::TempDir,
    pub(super) env: Env,
    pub(super) binding: GrantBinding,
    pub(super) auth: PathBuf,
    pub(super) bytes: Vec<u8>,
}

pub(super) fn fixture() -> Fixture {
    let world = aterm_tempfile::tempdir().unwrap();
    let home = world.path().join("home");
    let state = world.path().join("state");
    let config = world.path().join("config");
    let stage = world.path().join("stage");
    for directory in [&home, &state, &config, &stage] {
        aterm_secrets::fsx::ensure_private_dir(directory).unwrap();
    }
    client::machine_identity_as(
        &client::Paths::locate_for_home(&home),
        aterm_secrets::Making {
            test_binary: true,
            ..Default::default()
        },
    )
    .unwrap();
    let config_path = config.join("aterm.toml");
    std::fs::write(&config_path, "").unwrap();
    let env = Env {
        state,
        aterm_state: Some(home.join(".aterm")),
        cwd: home.clone(),
        home: Some(home),
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
        now: 1_800_000_000,
        sid: "s-maintenance-fixture".into(),
        sock: None,
        config: Some(config_path),
        claude_tmp: None,
        claude_running: None,
        tz_s: 0,
    };
    let roster = env.accounts_roster_for(accounts::Cli::Codex).unwrap();
    std::fs::write(
        &roster,
        format!(
            "[[account]]\nlabel='fixture'\nemail='fixture@example.test'\nclass='dedicated'\ndir='{}'\n",
            stage.display()
        ),
    )
    .unwrap();
    let request = codex_enrollment::Request {
        id: "a".repeat(32),
        caller: "b".repeat(64),
        label: "fixture".into(),
        revision: "c".repeat(64),
    };
    let auth = stage.join("auth.json");
    let bytes = document(env.now);
    let identity = Identity::from_native("fixture-account", "fixture-user");
    let mut prepared = prepare(&env, &request, &auth, &"d".repeat(64)).unwrap();
    prepared.begin_exchange().unwrap();
    prepared.capture_response(
        &bytes,
        Some(identity.clone()),
        Some("fixture-code-owner".into()),
    );
    let CaptureOutcome::Verified(retained) = prepared.settle_response() else {
        panic!("fixture response must settle")
    };
    private_file::write_private(&stage, "auth.json", &bytes).unwrap();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    store
        .transact_logins(env.now, &mut None, |_, logins| {
            logins.enrollment.admit_native(&request)?;
            Ok(Vec::new())
        })
        .unwrap();
    store
        .transact_logins(env.now, &mut None, |_, logins| {
            logins.enrollment.attach_grant(&request, &retained)?;
            Ok(Vec::new())
        })
        .unwrap();
    let binding = RefCell::new(None);
    store
        .transact_roster_logins_native_within(
            &[],
            &roster,
            env.now,
            &mut None,
            |_, state, logins| {
                let events = accounts::note_renewed(
                    state,
                    logins,
                    &request.label,
                    None,
                    stage.to_str().unwrap(),
                    None,
                    env.now,
                );
                logins.enrollment.publishing(&request)?;
                Ok(events)
            },
            |permit| {
                let actual = permit.binding(&request, &retained)?;
                let transition = initialize_revision(
                    &env,
                    &FreshActivationGuard {
                        binding: &actual,
                        identity: &identity,
                        document: &bytes,
                    },
                )?;
                *binding.borrow_mut() = Some(actual);
                Ok(Some(transition))
            },
        )
        .unwrap();
    let binding = binding.into_inner().unwrap();
    Fixture {
        _world: world,
        env,
        binding,
        auth,
        bytes,
    }
}

#[test]
fn canonical_maintenance_refuses_a_physical_auth_alias_without_changing_custody() {
    let fixture = fixture();
    let store = fixture
        .env
        .accounts_store_for(accounts::Cli::Codex)
        .unwrap();
    let lifetime = account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let cancellation = scope.cancellation();
    let entered = std::cell::Cell::new(0);
    let admit = || {
        with_maintenance_owner(&fixture.env, &fixture.binding, &scope, |_| {
            entered.set(entered.get() + 1);
            Ok(())
        })
    };
    let first = admit();
    let first_entered = entered.get();
    let records = std::fs::read(store.logins_file()).unwrap();
    let credential = std::fs::read(&fixture.auth).unwrap();
    let alias = fixture.auth.with_extension("fixture-alias");
    std::fs::hard_link(&fixture.auth, &alias).unwrap();
    let refused = admit();
    let refused_entered = entered.get();
    let records_unchanged = std::fs::read(store.logins_file()).unwrap() == records;
    let auth_unchanged = std::fs::read(&fixture.auth).unwrap() == credential;
    // Remove the actual alias before checking the refusal, including on regression.
    let removed = std::fs::remove_file(&alias);
    let restored = admit();
    let restored_entered = entered.get();
    drop(scope);
    let drained = lifetime.pause().unwrap().try_wait();
    first.unwrap();
    removed.unwrap();
    restored.unwrap();
    assert_eq!(
        first_entered, 1,
        "the genuine canonical reader admitted the original file"
    );
    assert!(refused.is_err());
    assert_eq!(
        refused_entered, first_entered,
        "an alias must refuse before the maintenance callback"
    );
    assert!(records_unchanged && auth_unchanged);
    assert_eq!(restored_entered, first_entered + 1);
    assert!(cancellation.retired());
    drained.expect("the original maintenance scope retired");
}

#[test]
fn cancelled_app_scope_can_admit_canonical_maintenance_without_consumer_delivery() {
    let fixture = fixture();
    let env = &fixture.env;
    let binding = &fixture.binding;
    let auth = &fixture.auth;
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let before = std::fs::read(store.logins_file()).unwrap();
    let lifetime = account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    scope.cancellation().cancel();
    assert!(scope.cancelled());
    with_maintenance_owner(env, binding, &scope, |mut grant| {
        assert_eq!(grant.canonical_auth(), auth);
        let witness = grant.anchor().read()?.ok_or("missing fixture witness")?;
        assert_eq!(witness.revision.generation, 1);
        Ok(())
    })
    .unwrap();
    let process = super::super::native_act::Process::observe(std::process::id()).unwrap();
    let caller = codex_session_slot::Caller {
        sid: env.sid.clone(),
        pid: process.pid,
        birth: process.birth,
    };
    assert!(
        with_current_published_grant(env, binding, &caller, |_| -> Result<(), String> {
            panic!("a native test process must not become a qualified Codex delivery")
        })
        .is_err()
    );
    assert!(
        std::fs::read(store.logins_file()).unwrap() == before,
        "maintenance changed canonical records"
    );
    assert!(
        std::fs::read(auth).unwrap() == fixture.bytes,
        "maintenance changed canonical credentials"
    );
    assert!(
        !store.dir.join("session-slots").exists(),
        "maintenance must not materialize a consumer"
    );
    assert!(!scope.cancellation().retired());
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Synthetic producer confined to a newly created private world. This supplies
//! native GUI recovery controls, never real provider or lineage qualification.
use super::*;
use crate::harness::accounts;
use aterm_secrets::client;

pub(in crate::harness) fn create(
    caller: &str,
) -> (aterm_tempfile::TempDir, Env, [codex_enrollment::Request; 2]) {
    assert!(caller.len() == 64 && caller.bytes().all(|b| b.is_ascii_hexdigit()));
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let stage = root.path().join("stage");
    aterm_secrets::fsx::ensure_private_dir(&home).unwrap();
    aterm_secrets::fsx::ensure_private_dir(&stage).unwrap();
    {
        client::machine_identity_as(
            &client::Paths::locate_for_home(&home),
            aterm_secrets::Making {
                test_binary: true,
                ..aterm_secrets::Making::default()
            },
        )
        .unwrap();
    }
    let mut env = Env {
        state: root.path().join("state"),
        aterm_state: Some(root.path().join("aterm")),
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
        now: 1_790_000_000,
        sid: "s-test-owner".into(),
        sock: None,
        config: None,
        claude_tmp: None,
        claude_running: None,
        tz_s: 0,
    };
    let world = root;
    let config = world.path().join("aterm.toml");
    std::fs::write(&config, "").unwrap();
    env.config = Some(config);
    let roster = env.accounts_roster_for(accounts::Cli::Codex).unwrap();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    store.ensure().unwrap();
    let old_a = world.path().join("old-a");
    let old_b = world.path().join("old-b");
    for path in [&old_a, &old_b] {
        aterm_secrets::fsx::ensure_private_dir(path).unwrap();
    }
    std::fs::write(&roster, format!(
        "[[account]]\nlabel='alpha'\nemail='alpha@example.test'\nclass='dedicated'\ndir={}\n[[account]]\nlabel='beta'\nemail='beta@example.test'\nclass='dedicated'\ndir={}\n",
        aterm_json::to_string(old_a.to_str().unwrap()).unwrap(),
        aterm_json::to_string(old_b.to_str().unwrap()).unwrap(),
    )).unwrap();
    let prepare_row = |label: &str, nonce: char| {
        let request = codex_enrollment::Request {
            id: nonce.to_string().repeat(32),
            caller: caller.to_owned(),
            label: label.into(),
            revision: "c".repeat(64),
        };
        store
            .transact_logins(env.now, &mut None, |_, logins| {
                logins.enrollment.admit_native(&request)?;
                Ok(Vec::new())
            })
            .unwrap();
        let directory = store.claim_staging_dir(label).unwrap();
        let held = store.acquire_bounded().unwrap().unwrap();
        store
            .transact_pool_logins_held(&held, &roster, env.now, &mut None, |_, _, logins| {
                accounts::admit_workflow(logins, label)?;
                Ok(Vec::new())
            })
            .unwrap();
        let snapshot = store
            .native_row_snapshot_held(&held, &roster, env.now)
            .unwrap();
        let records = snapshot.account_image();
        let parent = accounts::RenewalParent::capture(&records.logins, label);
        let workflow =
            accounts::current_workflow(records.raw_roster.row(label).unwrap(), &records.logins)
                .unwrap()
                .value();
        let occurrence =
            super::super::codex_native_row::NativeRowOccurrence::capture(snapshot, &request)
                .unwrap();
        let staging = codex_enrollment::Staging {
            native_row: Some(occurrence.fence().clone()),
            request: request.clone(),
            store: std::fs::canonicalize(&store.dir).unwrap(),
            directory: std::fs::canonicalize(&directory).unwrap(),
            store_identity: Some(codex_enrollment::DirectoryIdentity::capture(&store.dir).unwrap()),
            directory_identity: Some(
                codex_enrollment::DirectoryIdentity::capture(&directory).unwrap(),
            ),
            parent,
            workflow,
            policy: occurrence.policy().to_owned(),
            selection: None,
        };
        let admission = occurrence.finish(staging.clone()).unwrap();
        let artifact = std::cell::RefCell::new(None);
        store
            .transact_roster_logins_held(&held, &roster, env.now, &mut None, |_, _, logins| {
                *artifact.borrow_mut() = Some(
                    logins
                        .enrollment
                        .bind_staging(admission.staging().clone())?,
                );
                Ok(Vec::new())
            })
            .unwrap();
        drop(held);
        // Synthetic producer injection, real encrypted issuer and canonical
        // writer. This test proves row reconciliation, not provider identity,
        // independence, SourceV1, or browser/email acceptance.
        let claims = aterm_json::json!({"sub":label,"email":format!("{label}@example.test"),"exp":4_000_000_000u64,
            "https://api.openai.com/auth":{"chatgpt_account_id":label,"chatgpt_user_id":label}});
        let payload = aterm_codec::base64::encode(&aterm_json::to_vec(&claims).unwrap())
            .unwrap()
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_");
        let jwt = format!("e30.{payload}.c2ln");
        let document = aterm_json::to_string(&aterm_json::json!({"auth_mode":"chatgpt","last_refresh":env.now,
            "tokens":{"account_id":label,"id_token":jwt,"access_token":jwt,"refresh_token":format!("test-refresh-{label}"),"login_owner_id":format!("test-owner-{label}")}})).unwrap();
        let mut pending = prepare(
            &env,
            &request,
            &directory.join("auth.json"),
            &"d".repeat(64),
        )
        .unwrap();
        pending.begin_exchange().unwrap();
        pending.capture_response(
            document.as_bytes(),
            Some(Identity::from_native(label, label)),
            Some(format!("test-owner-{label}")),
        );
        pending.record.schema = 6;
        let response = pending.record.response.as_mut().unwrap();
        response.predecessor = Some(StagePredecessor { digest: None });
        response.native_row = Some(staging.clone());
        let CaptureOutcome::Verified(fresh) = pending.settle_response() else {
            panic!("fixture retention failed")
        };
        store
            .transact_logins(env.now, &mut None, |_, logins| {
                logins.enrollment.attach_grant(&request, &fresh)?;
                Ok(Vec::new())
            })
            .unwrap();
        (request, staging, fresh, artifact.into_inner())
    };
    let rows = [prepare_row("alpha", 'a'), prepare_row("beta", 'e')];
    let [(a, _, _, _), (b, _, _, _)] = rows;
    (world, env, [a, b])
}

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
    create_with_lineage(
        caller,
        [
            ("alpha", "alpha", "test-refresh-alpha"),
            ("beta", "beta", "test-refresh-beta"),
        ],
    )
}

// Synthetic issuer inputs confined to the newly allocated world. These are
// algorithm controls, never provider independence or production authority.
pub(super) fn create_with_lineage(
    caller: &str,
    bundles: [(&str, &str, &str); 2],
) -> (aterm_tempfile::TempDir, Env, [codex_enrollment::Request; 2]) {
    create_with_lineage_observed(caller, bundles, |_| {})
}

pub(super) fn create_with_lineage_observed(
    caller: &str,
    bundles: [(&str, &str, &str); 2],
    between: impl FnOnce(&Env),
) -> (aterm_tempfile::TempDir, Env, [codex_enrollment::Request; 2]) {
    create_rows(
        caller,
        [("alpha", 'a', bundles[0]), ("beta", 'e', bundles[1])],
        between,
    )
}

#[cfg(test)]
pub(in crate::harness) fn create_five(
    caller: &str,
) -> (aterm_tempfile::TempDir, Env, [codex_enrollment::Request; 5]) {
    create_rows(
        caller,
        [
            ("alpha", '1', ("alpha", "alpha", "refresh-one")),
            ("beta", '2', ("beta", "beta", "refresh-two")),
            ("gamma", '3', ("gamma", "gamma", "refresh-three")),
            ("delta", '4', ("delta", "delta", "refresh-four")),
            ("epsilon", '5', ("epsilon", "epsilon", "refresh-five")),
        ],
        |_| {},
    )
}

type SyntheticRow<'a> = (&'a str, char, (&'a str, &'a str, &'a str));

fn create_rows<const N: usize>(
    caller: &str,
    rows: [SyntheticRow<'_>; N],
    between: impl FnOnce(&Env),
) -> (aterm_tempfile::TempDir, Env, [codex_enrollment::Request; N]) {
    create_rows_in(caller, rows, between, Layout::Unit)
}

#[derive(Clone, Copy)]
enum Layout {
    Unit,
    SourceHost,
}

/// Only this constructor chooses the native lab layout. No existing path or
/// credential can be supplied by its caller.
pub(in crate::harness) fn create_source_host(
    caller: &str,
) -> (aterm_tempfile::TempDir, Env, [codex_enrollment::Request; 2]) {
    create_rows_in(
        caller,
        [
            ("alpha", 'a', ("alpha", "alpha", "test-refresh-alpha")),
            ("beta", 'e', ("beta", "beta", "test-refresh-beta")),
        ],
        |_| {},
        Layout::SourceHost,
    )
}

fn create_rows_in<const N: usize>(
    caller: &str,
    rows: [SyntheticRow<'_>; N],
    between: impl FnOnce(&Env),
    layout: Layout,
) -> (aterm_tempfile::TempDir, Env, [codex_enrollment::Request; N]) {
    assert!(caller.len() == 64 && caller.bytes().all(|b| b.is_ascii_hexdigit()));
    let root = match layout {
        Layout::Unit => aterm_tempfile::tempdir().unwrap(),
        Layout::SourceHost => aterm_tempfile::Builder::new()
            .prefix("atsp")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .unwrap(),
    };
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
    let state_home = match layout {
        Layout::Unit => root.path().join("aterm"),
        Layout::SourceHost => home.join(".aterm"),
    };
    let mut env = Env {
        state: match layout {
            Layout::Unit => root.path().join("state"),
            Layout::SourceHost => state_home.join("harness").join(super::super::cli::HARNESS),
        },
        aterm_state: Some(state_home),
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
        xdg_config_home: None,
        codex_login: None,
        agy_conversation: None,
        now: match layout {
            Layout::Unit => 1_790_000_000,
            Layout::SourceHost => crate::supervise::limit::unix_now(),
        },
        sid: "s-test-owner".into(),
        sock: None,
        config: None,
        claude_tmp: None,
        claude_running: None,
        tz_s: 0,
    };
    let world = root;
    let config = match layout {
        Layout::Unit => world.path().join("aterm.toml"),
        Layout::SourceHost => world.path().join("cfg/aterm/aterm.toml"),
    };
    if matches!(layout, Layout::SourceHost) {
        aterm_secrets::fsx::ensure_private_dir(&world.path().join("cfg")).unwrap();
    }
    aterm_secrets::fsx::ensure_private_dir(config.parent().unwrap()).unwrap();
    let configuration = match layout {
        Layout::Unit => "",
        Layout::SourceHost => {
            "agents_auto_prime = false\n[update]\nenabled = false\nauto_apply = false\n[packages]\nenabled = false\n[harness]\nenabled = false\naccounts = 'wall'\n"
        }
    };
    std::fs::write(&config, configuration).unwrap();
    env.config = Some(config);
    let roster = env.accounts_roster_for(accounts::Cli::Codex).unwrap();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    store.ensure().unwrap();
    let mut text = String::new();
    for (label, _, _) in &rows {
        let old = world.path().join(format!("old-{label}"));
        aterm_secrets::fsx::ensure_private_dir(&old).unwrap();
        text.push_str(&format!(
            "[[account]]\nlabel={0}\nemail={1}\nclass='dedicated'\ndir={2}\n",
            aterm_json::to_string(label).unwrap(),
            aterm_json::to_string(&format!("{label}@example.test")).unwrap(),
            aterm_json::to_string(old.to_str().unwrap()).unwrap(),
        ));
    }
    std::fs::write(&roster, text).unwrap();
    let mut between = Some(between);
    let requests = rows.map(|(label, nonce, bundle)| {
        let request = prepare_row(&env, caller, label, nonce, bundle);
        if let Some(between) = between.take() {
            between(&env);
        }
        request
    });
    (world, env, requests)
}

fn stage_row(
    env: &Env,
    caller: &str,
    label: &str,
    nonce: char,
) -> (
    codex_enrollment::Request,
    super::super::codex_native_row::NativeRowAdmission,
    PathBuf,
    codex_enrollment::Artifact,
) {
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let roster = env.accounts_roster_for(accounts::Cli::Codex).unwrap();
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
        super::super::codex_native_row::NativeRowOccurrence::capture(snapshot, &request).unwrap();
    let staging = codex_enrollment::Staging {
        native_row: Some(occurrence.fence().clone()),
        request: request.clone(),
        store: std::fs::canonicalize(&store.dir).unwrap(),
        directory: std::fs::canonicalize(&directory).unwrap(),
        store_identity: Some(codex_enrollment::DirectoryIdentity::capture(&store.dir).unwrap()),
        directory_identity: Some(codex_enrollment::DirectoryIdentity::capture(&directory).unwrap()),
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
    (
        request,
        admission,
        directory,
        artifact.into_inner().unwrap(),
    )
}

pub(super) fn prepare_row(
    env: &Env,
    caller: &str,
    label: &str,
    nonce: char,
    bundle: (&str, &str, &str),
) -> codex_enrollment::Request {
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let (request, admission, directory, _artifact) = stage_row(env, caller, label, nonce);
    let staging = admission.staging().clone();
    let (account, user, refresh) = bundle;
    // Synthetic producer injection, real encrypted issuer and canonical
    // writer. This test proves row reconciliation, not provider identity,
    // independence, SourceV1, or browser/email acceptance.
    let claims = aterm_json::json!({"sub":label,"email":format!("{label}@example.test"),"exp":4_000_000_000u64,
        "https://api.openai.com/auth":{"chatgpt_account_id":account,"chatgpt_user_id":user}});
    let payload = aterm_codec::base64::encode(&aterm_json::to_vec(&claims).unwrap())
        .unwrap()
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_");
    let jwt = format!("e30.{payload}.c2ln");
    let document = aterm_json::to_string(&aterm_json::json!({"auth_mode":"chatgpt","last_refresh":aterm_types::rfc3339::format_rfc3339(u64::try_from(env.now).unwrap()),
        "tokens":{"account_id":account,"id_token":jwt,"access_token":jwt,"refresh_token":refresh,"login_owner_id":format!("test-owner-{label}"),"oauth_provenance":{"issuer":codex_app_login::DEFAULT_ISSUER,"client_id":codex_app_login::CLIENT_ID}}})).unwrap();
    let mut pending =
        prepare(env, &request, &directory.join("auth.json"), &"d".repeat(64)).unwrap();
    pending.begin_exchange().unwrap();
    pending.capture_response(
        document.as_bytes(),
        Some(Identity::from_native(account, user)),
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
    request
}

/// A second real canonical source store shares only this owned world's machine
/// home/identity/catalogue. No caller home/path or existing grant is accepted.
#[cfg(test)]
pub(super) fn create_second_state(
    caller: &str,
    bundle: (&str, &str, &str),
) -> (
    aterm_tempfile::TempDir,
    Env,
    Env,
    [codex_enrollment::Request; 2],
    codex_enrollment::Request,
) {
    let (world, env, requests) = create(caller);
    let mut second = env.clone();
    second.state = world.path().join("second-state");
    let config = world.path().join("second-config");
    aterm_secrets::fsx::ensure_private_dir(&config).unwrap();
    let config_file = config.join("aterm.toml");
    std::fs::write(&config_file, "").unwrap();
    second.config = Some(config_file);
    let store = second.accounts_store_for(accounts::Cli::Codex).unwrap();
    store.ensure().unwrap();
    let old = world.path().join("old-gamma");
    aterm_secrets::fsx::ensure_private_dir(&old).unwrap();
    std::fs::write(
        second.accounts_roster_for(accounts::Cli::Codex).unwrap(),
        format!(
            "[[account]]\nlabel='gamma'\nemail='gamma@example.test'\nclass='dedicated'\ndir={}\n",
            aterm_json::to_string(old.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    let request = prepare_row(&second, caller, "gamma", 'f', bundle);
    (world, env, second, requests, request)
}

/// Actual new-format code producer fixture. All paths and keys belong to its
/// fresh private world; this grants no image qualification or provider-family
/// independence. The callback performs the real CodeExchange itself.
#[cfg(test)]
pub(super) fn with_global_code_fixture(
    test: impl FnOnce(
        &Env,
        &codex_enrollment::Request,
        &Path,
        std::sync::Arc<super::super::native_enrollment_admission::NativeBrowserTurn>,
        PreparedGrant,
        codex_enrollment::Artifact,
    ),
) {
    with_preparation_fixture(|env, request, auth, turn, admission, artifact| {
        let prepared = prepare(env, request, auth, &"d".repeat(64))
            .unwrap()
            .with_native_scope(env, Some(turn.clone()))
            .unwrap()
            .with_row_admission(admission)
            .unwrap();
        test(env, request, auth, turn, prepared, artifact);
    });
}

#[cfg(test)]
pub(in crate::harness) fn with_preparation_fixture(
    test: impl FnOnce(
        &Env,
        &codex_enrollment::Request,
        &Path,
        std::sync::Arc<super::super::native_enrollment_admission::NativeBrowserTurn>,
        super::super::codex_native_row::NativeRowAdmission,
        codex_enrollment::Artifact,
    ),
) {
    let (world, env, _) = create(&"d".repeat(64));
    let old = world.path().join("old-global-code");
    aterm_secrets::fsx::ensure_private_dir(&old).unwrap();
    let roster_path = env.accounts_roster_for(accounts::Cli::Codex).unwrap();
    let mut roster = std::fs::read_to_string(&roster_path).unwrap();
    roster.push_str(&format!(
        "[[account]]\nlabel='global-code'\nemail='global-code@example.test'\nclass='dedicated'\ndir={}\n",
        aterm_json::to_string(old.to_str().unwrap()).unwrap()));
    std::fs::write(&roster_path, roster).unwrap();
    let (request, admission, directory, artifact) =
        stage_row(&env, &"d".repeat(64), "global-code", 'f');
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let roster = accounts::Roster::parse_for(
        accounts::Cli::Codex,
        &std::fs::read_to_string(&roster_path).unwrap(),
    )
    .unwrap();
    let turn =
        super::super::native_enrollment_admission::NativeBrowserTurn::acquire_isolated_fixture(
            &store,
            &request,
            roster.row(&request.label).unwrap(),
            env.now,
        )
        .unwrap();
    let auth = std::fs::canonicalize(&directory).unwrap().join("auth.json");
    test(&env, &request, &auth, turn, admission, artifact);
}

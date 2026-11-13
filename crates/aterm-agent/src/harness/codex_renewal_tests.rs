// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The tests gated `cfg(unix)` here observe a process (`native_act::Process::observe`), which
// only Unix reads.

use super::*;
const THREAD: &str = "019a4b54-5fa1-7123-8123-123456789abc";
const WALL: &str = "019a4b54-5fa2-7123-8123-123456789abc";
const OTHER: &str = "019a4b54-5fa3-7123-8123-123456789abc";

fn intent() -> Intent {
    Intent {
        purpose: Default::default(),
        thread: THREAD.into(),
        wall: WALL.into(),
        identity: Identity::from_native("workspace", "user"),
    }
}

#[test]
fn protocol_admits_only_exact_bounded_fields() {
    let intent = intent();
    let line = format!(
        "prepare thread={THREAD} wall={WALL} account={} user={}",
        intent.identity.account, intent.identity.user
    );
    assert_eq!(Request::parse(&line), Some(Request::Prepare(intent)));
    for extra in [" path=/tmp/auth", " thread=x", " extra=x"] {
        assert!(Request::parse(&(line.clone() + extra)).is_none());
    }
    assert!(Request::parse("commit id=../file").is_none());
    assert!(Request::parse("status id=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").is_none());
}

#[test]
fn identity_proofs_are_domain_separated_and_disclose_no_identifiers() {
    let identity = Identity::from_native("same", "same");
    assert_ne!(identity.account, identity.user);
    assert_eq!(identity.account.len(), 64);
    assert!(!format!("{identity:?}").contains("same"));
    assert_ne!(identity, Identity::from_native("other", "same"));
}

fn event(value: aterm_json::Value) -> String {
    format!(
        "{}\n",
        aterm_json::json!({"type":"event_msg","payload":value})
    )
}

#[test]
fn native_history_requires_latest_completed_auth_failure() {
    let start = event(aterm_json::json!({"type":"task_started","turn_id":WALL}));
    let error = event(aterm_json::json!({"type":"error","codex_error_info":"unauthorized"}));
    let done = event(aterm_json::json!({"type":"task_complete","turn_id":WALL}));
    let text = format!("{start}{error}{done}");
    assert_eq!(
        history_lines(text.as_bytes(), &intent()).unwrap(),
        History {
            starts: 1,
            auth_wall: true,
            usage_wall: false,
            latest_is_wall: true
        }
    );
    let later = text + &event(aterm_json::json!({"type":"task_started","turn_id":OTHER}));
    assert!(
        !history_lines(later.as_bytes(), &intent())
            .unwrap()
            .auth_wall
    );
    assert!(
        !history_lines(format!("{start}{error}").as_bytes(), &intent())
            .unwrap()
            .auth_wall
    );
}

#[test]
fn terminal_typed_401_is_login_but_other_http_failures_are_not() {
    let start = event(aterm_json::json!({"type":"task_started", "turn_id":WALL}));
    for kind in [
        "http_connection_failed",
        "response_stream_connection_failed",
        "response_too_many_failed_attempts",
        "response_stream_disconnected",
    ] {
        for (status, expected) in [
            ("401", true),
            ("403", false),
            ("null", false),
            ("\"401\"", false),
        ] {
            let error: aterm_json::Value = aterm_json::from_str(&format!(
                "{{\"codex_error_info\":{{\"{kind}\":{{\"http_status_code\":{status}}}}}}}"
            ))
            .unwrap();
            let done =
                event(aterm_json::json!({"type":"task_complete", "turn_id":WALL, "error":error}));
            assert_eq!(
                history_lines(format!("{start}{done}").as_bytes(), &intent())
                    .unwrap()
                    .auth_wall,
                expected
            );
        }
    }
    for bad in [
        aterm_json::json!({"codex_error_info":{"other":{"http_status_code":401}}}),
        aterm_json::json!({"codex_error_info":{"http_connection_failed":{"http_status_code":401},"unauthorized":null}}),
        aterm_json::json!({"codex_error_info":{"http_connection_failed":{"http_status_code":401,"extra":true}}}),
        aterm_json::json!({"codex_error_info":"other","message":"401 unauthorized"}),
    ] {
        assert!(!native_login_error(&bad));
    }
    let previous = event(aterm_json::json!({"type":"error", "codex_error_info":"unauthorized"}));
    let success = event(aterm_json::json!({"type":"task_complete", "turn_id":WALL, "error":null}));
    assert!(
        !history_lines(format!("{start}{previous}{success}").as_bytes(), &intent())
            .unwrap()
            .auth_wall
    );
}

#[test]
fn repeated_native_turn_is_counted_before_receipt_consumption() {
    let text = event(aterm_json::json!({"type":"task_started","turn_id":WALL}))
        + &event(aterm_json::json!({"type":"turn_started","turn_id":WALL}));
    assert_eq!(history_lines(text.as_bytes(), &intent()).unwrap().starts, 2);
}

#[test]
fn quota_requests_and_markers_cannot_be_used_as_same_account_renewals() {
    let mut quota = intent();
    quota.purpose = Purpose::WeeklyQuota;
    let request = format!(
        "prepare thread={THREAD} turn={WALL} source_account={} source_user={}",
        quota.identity.account, quota.identity.user,
    );
    assert_eq!(
        Request::parse_rotation(&request),
        Some(Request::Prepare(quota.clone()))
    );
    assert!(Request::parse(&request).is_none());
    assert!(Request::parse_rotation(&(request + " target=/arbitrary/login")).is_none());
    let record = aterm_json::json!({
        "sessionId": THREAD, "status": "waiting", "draftEmpty": true,
        "autopilot": "active", "owed": {
            "thread": THREAD, "mode": "cruise", "rule": "usage-rotate@v1",
            "nativeOrigin": {"turnId": WALL, "phase": "complete"},
            "rotation": {"openaiScope": true, "account": quota.identity.account,
                "user": quota.identity.user, "receipt": null}
        }
    });
    assert!(quota.matches_record(&record, None));
    assert!(!intent().matches_record(&record, None));
}

#[test]
fn quota_history_is_typed_and_separate_from_authentication_failure() {
    let mut quota = intent();
    quota.purpose = Purpose::WeeklyQuota;
    let start = event(aterm_json::json!({"type":"task_started", "turn_id": WALL}));
    let error =
        event(aterm_json::json!({"type":"error", "codex_error_info":"usage_limit_exceeded"}));
    let done = event(aterm_json::json!({"type":"task_complete", "turn_id": WALL}));
    let history = history_lines(format!("{start}{error}{done}").as_bytes(), &quota).unwrap();
    assert!(quota.matches_history(&history));
    assert!(!intent().matches_history(&history));
    let unfinished = history_lines(format!("{start}{error}").as_bytes(), &quota).unwrap();
    assert!(!quota.matches_history(&unfinished));
    let prose = event(aterm_json::json!({"type":"error", "message":"usage_limit_exceeded"}));
    let untyped = history_lines(format!("{start}{prose}{done}").as_bytes(), &quota).unwrap();
    assert!(!quota.matches_history(&untyped));
}

fn auth(home: &Path, workspace: &str, user: Option<&str>, explicit: Option<&str>) {
    std::fs::create_dir_all(home).unwrap();
    let claims = aterm_json::json!({"https://api.openai.com/auth": {
        "chatgpt_account_id": workspace, "chatgpt_user_id": user
    }});
    let encoded = aterm_codec::base64::encode(claims.to_string().as_bytes())
        .unwrap()
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_owned();
    let token = format!("e30.{encoded}.fixture");
    std::fs::write(
        home.join("auth.json"),
        aterm_json::json!({"auth_mode":"chatgpt", "tokens": {
            "id_token":token, "access_token":token, "refresh_token":"fixture", "account_id":explicit
        }})
        .to_string(),
    )
    .unwrap();
}

#[test]
fn native_workspace_selection_matches_codex_token_precedence() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    auth(
        tmp.path(),
        "default-workspace",
        Some("user"),
        Some("selected-workspace"),
    );
    assert_eq!(
        codex_app_login::renewal_identity(tmp.path()),
        Some(Identity::from_native("selected-workspace", "user"))
    );
    auth(tmp.path(), "default-workspace", None, None);
    assert!(codex_app_login::renewal_identity(tmp.path()).is_none());
}

#[test]
fn successor_requires_same_native_identity_current_generation_and_no_hold() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let mut env = Env::from_process().unwrap();
    env.home = Some(tmp.path().join("home"));
    env.state = tmp.path().join("state");
    env.config = Some(tmp.path().join("aterm.toml"));
    let old = tmp.path().join("old");
    let new = tmp.path().join("new");
    auth(&old, "workspace", Some("user"), None);
    auth(&new, "workspace", Some("user"), None);
    let raw = format!(
        "[[account]]\nlabel='main'\nclass='dedicated'\ndir={}\nemail='owner@example'\n",
        aterm_toml::Value::from(old.to_str().unwrap())
    );
    std::fs::write(env.accounts_roster_for(accounts::Cli::Codex).unwrap(), &raw).unwrap();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    std::fs::create_dir_all(&store.dir).unwrap();
    let mut logins = accounts::Logins {
        generation_clock: 2,
        labels: std::collections::BTreeMap::from([(
            "main".into(),
            accounts::Binding {
                dir: Some(new.to_string_lossy().into_owned()),
                was: vec![Some(old.to_string_lossy().into_owned())],
                renewal_generation: 2,
                last_success_generation: 2,
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    std::fs::write(store.logins_file(), logins.to_json()).unwrap();
    let identity = Identity::from_native("workspace", "user");
    assert_eq!(
        with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)),
        Ok(Some(true))
    );
    let selected = with_successor(&env, &old.join("auth.json"), &identity, |next| {
        Ok(next.clone())
    })
    .unwrap()
    .unwrap();
    let mut prepared = receipt();
    prepared.old_auth = selected.old_auth.clone();
    prepared.new_auth = selected.new_auth.clone();
    prepared.intent.identity = selected.identity.clone();
    prepared.row_stamp = row_stamp(&selected.row);
    write_receipt(&env, &prepared).unwrap();
    let start = std::sync::Barrier::new(3);
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            start.wait();
            commit_receipt(&env, &prepared)
        });
        let second = scope.spawn(|| {
            start.wait();
            commit_receipt(&env, &prepared)
        });
        start.wait();
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Ok(Some(_))))
            .count(),
        1
    );
    assert_eq!(
        read_receipt(&env, &prepared.id).unwrap().phase,
        Phase::Committing
    );
    // The actual account writer serializes stale-Ready retirement against
    // late Commit and another fresh preparation, without renewing a login.
    let mut stale = prepared.clone();
    stale.id = "cd".repeat(16);
    stale.native_prepare = "immutable-original-prepare".into();
    with_successor(&env, &stale.old_auth, &identity, |_| {
        write_receipt(&env, &stale)
    })
    .unwrap()
    .unwrap();
    let start = std::sync::Barrier::new(3);
    let retired = std::thread::scope(|scope| {
        let retire = || {
            start.wait();
            with_successor(&env, &stale.old_auth, &identity, |next| {
                let mut current = read_receipt(&env, &stale.id).ok_or("stale-receipt")?;
                if current != stale
                    || !current.matches_successor(next)
                    || !current.retire_stale_ready(4, 4, 50)
                {
                    return Ok(false);
                }
                write_receipt(&env, &current)?;
                Ok(true)
            })
        };
        let first = scope.spawn(retire);
        let second = scope.spawn(retire);
        start.wait();
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(
        retired
            .iter()
            .filter(|outcome| matches!(outcome, Ok(Some(true))))
            .count(),
        1
    );
    assert!(
        commit_receipt(&env, &stale).is_err(),
        "late Commit cannot revive the retired preparation"
    );
    let retired = read_receipt(&env, &stale.id).unwrap();
    assert_eq!(retired.phase, Phase::Retired);
    assert_eq!(retired.native_prepare, stale.native_prepare);
    assert_eq!(retired.expires_ms, stale.expires_ms);
    assert!(!retired.launch_attempted);
    let mut fresh = stale.clone();
    fresh.id = "ef".repeat(16);
    fresh.claim_revision = 4;
    fresh.expires_ms = 200;
    fresh.native_prepare = "distinct-current-prepare".into();
    with_successor(&env, &fresh.old_auth, &identity, |next| {
        assert!(fresh.matches_successor(next));
        write_receipt(&env, &fresh)
    })
    .unwrap()
    .unwrap();
    assert!(commit_receipt(&env, &fresh).unwrap().is_some());
    assert_eq!(read_receipt(&env, &stale.id).unwrap(), retired);
    assert_eq!(
        store.load_logins(env.now).unwrap().0.generation_clock,
        2,
        "resealing reuses the same valid renewed credential generation"
    );
    logins
        .labels
        .get_mut("main")
        .unwrap()
        .last_success_generation = 1;
    std::fs::write(store.logins_file(), logins.to_json()).unwrap();
    assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
    logins
        .labels
        .get_mut("main")
        .unwrap()
        .last_success_generation = 2;
    std::fs::write(store.logins_file(), logins.to_json()).unwrap();
    auth(&new, "workspace", Some("other-user"), None);
    assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
    auth(&new, "other-workspace", Some("user"), None);
    assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
    auth(&new, "workspace", Some("user"), None);
    crate::test_config::write(env.config.as_ref().unwrap(), "[harness]\nlogins='off'\n").unwrap();
    assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
    for setting in ["relaunch", "resume_limits"] {
        crate::test_config::write(
            env.config.as_ref().unwrap(),
            format!("[harness]\n{setting}=false\n"),
        )
        .unwrap();
        assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
    }
    crate::test_config::write(
        env.config.as_ref().unwrap(),
        "[harness.codex]\nenabled=false\n",
    )
    .unwrap();
    assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
    crate::test_config::write(env.config.as_ref().unwrap(), "[harness").unwrap();
    assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
    crate::test_config::write(env.config.as_ref().unwrap(), "").unwrap();
    std::fs::write(
        env.accounts_roster_for(accounts::Cli::Codex).unwrap(),
        raw + "held_by='m2'\n",
    )
    .unwrap();
    assert!(with_successor(&env, &old.join("auth.json"), &identity, |_| Ok(true)).is_err());
}
fn receipt() -> Receipt {
    Receipt {
        destination: None,
        id: "ab".repeat(16),
        intent: intent(),
        sid: "s-fixture".into(),
        old_pid: 10,
        old_birth: 20,
        claim_revision: 3,
        human_seq: 4,
        old_auth: "/old/auth.json".into(),
        new_auth: "/new/auth.json".into(),
        generation: 2,
        row_stamp: String::new(),
        native_prepare: String::new(),
        expires_ms: 100,
        phase: Phase::Ready,
        successor_pid: None,
        successor_birth: None,
        accepted_turn: None,
        launch_attempted: false,
        candidate: None,
        current_attempt: None,
        revoked: false,
    }
}

#[test]
fn receipt_requires_an_explicit_distinct_quota_destination() {
    let original = receipt();
    assert_eq!(original.target_identity(), Some(&original.intent.identity));
    let value = aterm_json::to_value(&original).unwrap();
    assert!(value.get("destination").is_none());
    assert!(value["intent"].get("purpose").is_none());
    assert_eq!(aterm_json::from_value::<Receipt>(value).unwrap(), original);

    let target = Identity::from_native("next-workspace", "next-user");
    let mut quota = original.clone();
    quota.intent.purpose = Purpose::WeeklyQuota;
    assert!(quota.target_identity().is_none());
    quota.destination = Some(quota.intent.identity.clone());
    assert!(quota.target_identity().is_none());
    quota.destination = Some(target.clone());
    assert_eq!(quota.target_identity(), Some(&target));
    let reply = quota.reply();
    assert!(reply.starts_with("OK codex-rotation state=ready "));
    assert_eq!(reply.split_whitespace().count(), 10);
    assert!(reply.contains(&format!("target_account={}", target.account)));
    assert!(!reply.contains("/old/") && !reply.contains("/new/"));
    let mut changed = quota.clone();
    changed.destination = Some(Identity::from_native("third", "user"));
    assert!(!quota.same_intent(&changed));
    let mut renewal = original;
    renewal.destination = Some(target);
    assert!(renewal.target_identity().is_none());
}

#[test]
fn stale_ready_retires_only_unlaunched_preparations_and_keeps_the_original_recipe() {
    let original = receipt();
    assert!(!original.clone().retire_stale_ready(3, 4, 99));
    for (revision, human, now) in [(4, 4, 50), (3, 5, 50), (3, 4, 100)] {
        let mut stale = original.clone();
        assert!(stale.retire_stale_ready(revision, human, now));
        let mut expected = original.clone();
        expected.phase = Phase::Retired;
        assert_eq!(stale, expected);
    }
    for phase in [
        Phase::Committing,
        Phase::Committed,
        Phase::Consumed,
        Phase::Retired,
    ] {
        let mut active = original.clone();
        active.phase = phase;
        let before = active.clone();
        assert!(!active.retire_stale_ready(4, 5, 200));
        assert_eq!(active, before);
    }
    let mut attempted = original.clone();
    attempted.launch_attempted = true;
    assert!(!attempted.retire_stale_ready(4, 5, 200));
    let mut accepted = original;
    accepted.accepted_turn = Some(WALL.into());
    assert!(!accepted.retire_stale_ready(4, 5, 200));
    let mut revoked = receipt();
    revoked.revoked = true;
    assert!(!revoked.retire_stale_ready(4, 5, 200));
    let mut attempted = receipt();
    attempted.current_attempt = Some(AttemptRef {
        id: "12".repeat(16),
        generation: 1,
    });
    assert!(!attempted.retire_stale_ready(4, 5, 200));
}

#[test]
fn receipt_consumption_is_one_native_turn_and_only_after_commit() {
    let mut receipt = receipt();
    assert!(!receipt.consume(WALL));
    receipt.phase = Phase::Committing;
    assert!(!receipt.consume(WALL));
    receipt.phase = Phase::Committed;
    assert!(!receipt.consume(OTHER));
    assert!(receipt.consume(WALL));
    assert!(receipt.consume(WALL));
    assert!(!receipt.consume(OTHER));
}

#[test]
fn proven_no_signal_recovers_but_unknown_launch_cannot_restart() {
    let mut receipt = receipt();
    receipt.phase = Phase::Committing;
    assert!(receipt.reset_before_signal(90));
    assert_eq!(receipt.phase, Phase::Ready);
    receipt.phase = Phase::Committing;
    assert!(receipt.reset_before_signal(100));
    assert!(receipt.may_replace());
    receipt.phase = Phase::Committing;
    receipt.launch_attempted = true;
    assert!(!receipt.reset_before_signal(100));
    assert!(!receipt.may_replace());
}

#[test]
fn every_known_reply_is_identity_bound_without_native_paths() {
    for phase in [
        Phase::Ready,
        Phase::Committing,
        Phase::Committed,
        Phase::Consumed,
        Phase::Retired,
    ] {
        let mut receipt = receipt();
        receipt.phase = phase;
        let reply = receipt.reply();
        assert_eq!(reply.split_whitespace().count(), 8);
        assert!(reply.contains(&receipt.intent.identity.account));
        assert!(!reply.contains("/old/") && !reply.contains("/new/"));
    }
    assert_eq!(
        Identity::from_native("same", "same").account,
        "4d9a5fa2af08763c11df9b82406d3b53813e1d0e957f9f682bb451234e54ce46"
    );
    assert_eq!(
        Identity::from_native("same", "same").user,
        "002317753bb35b3537fca8bdfaa8c0912d9c3912d607e1ead5afca2482c08814"
    );
}

#[test]
fn persisted_intent_cannot_override_pause_draft_or_other_native_point() {
    let intent = intent();
    let record = |autopilot: &str, draft_empty: bool, wall: &str| {
        aterm_json::json!({"sessionId":THREAD,"status":"waiting","draftEmpty":draft_empty,"autopilot":autopilot,
            "owed":{"thread":THREAD,"mode":"land","rule":"login-back@v1","nativeOrigin":{"turnId":wall,"phase":"complete"},
            "renewal":{"account":intent.identity.account,"user":intent.identity.user,"receipt":null,"openaiScope":true}}})
    };
    assert!(intent.matches_record(&record("active", true, WALL), None));
    assert!(!intent.matches_record(&record("off", true, WALL), None));
    assert!(!intent.matches_record(&record("active", false, WALL), None));
    assert!(!intent.matches_record(&record("active", true, OTHER), None));
    for scope in [
        aterm_json::Value::Null,
        aterm_json::json!(false),
        aterm_json::json!("true"),
    ] {
        let mut unscoped = record("active", true, WALL);
        unscoped
            .as_object_mut()
            .unwrap()
            .get_mut("owed")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("renewal")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("openaiScope".into(), scope);
        assert!(!intent.matches_record(&unscoped, None));
    }
    let mut absent = record("active", true, WALL);
    absent
        .as_object_mut()
        .unwrap()
        .get_mut("owed")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("renewal")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("openaiScope");
    assert!(!intent.matches_record(&absent, None));
    let local = aterm_json::json!({"sessionId":THREAD,"status":"waiting","draftEmpty":true,"autopilot":"active",
        "owed":{"thread":THREAD,"mode":"land","rule":"login-back@v1","nativeOrigin":{"turnId":WALL,"phase":"complete"},
        "renewal":{"account":intent.identity.account,"user":intent.identity.user,"receipt":null,"local":true,"openaiScope":true}}});
    assert!(!intent.matches_record(&local, None));
}
#[cfg(unix)]
#[test]
fn attached_launcher_requires_positive_exact_exec_image_before_recovery() {
    use super::super::native_act::{Image, NativePlan, Prepare, Process, RestartCommand};
    let launcher = Image {
        path: b"/host".to_vec(),
        device: 1,
        inode: 1,
    };
    let target = Image {
        path: b"/codex".to_vec(),
        device: 1,
        inode: 2,
    };
    let attached = Process {
        pid: 50,
        birth: 70,
        parent: 20,
        group: 50,
        session: 20,
        image: launcher.clone(),
    };
    let shell = Process {
        pid: 20,
        birth: 60,
        parent: 1,
        group: 20,
        session: 20,
        image: launcher.clone(),
    };
    let command = RestartCommand {
        settles: None,
        account_intent: None,
        image: target.clone(),
        program: "/codex".into(),
        argv: vec![],
        env: vec![],
        unset: None,
        cwd: None,
        heal: None,
        dialect: super::super::upgrade::Dialect::Zsh,
    };
    let mut prepare = Prepare {
        receipt_mode: None,
        rejection_domain: None,
        asked: None,
        id: "ab".repeat(16),
        host_epoch: "cd".repeat(16),
        holder: "fixture".into(),
        revision: 1,
        expected_human_seq: Some(0),
        deadline_us: 100,
        duty: super::super::contract::Scope::Relaunch,
        plan: NativePlan::Restart {
            process: attached.clone(),
            observation: None,
            shell: shell.clone(),
            launcher: launcher.clone(),
            socket: "/socket".into(),
            forward: Box::new(command),
            cold: None,
            rollback: None,
        },
        boot: None,
        custody: None,
    };
    let mut receipt = receipt();
    receipt.candidate = Some(Candidate::of(&attached, true));
    assert_eq!(receipt.exec_candidate(None, &prepare), None);
    assert_eq!(receipt.exec_candidate(Some(&attached), &prepare), None);
    let mut executed = Process {
        image: target,
        ..attached
    };
    assert_eq!(
        receipt.exec_candidate(Some(&executed), &prepare),
        Some(true)
    );
    let mut wrapped = prepare.clone();
    if let NativePlan::Restart { forward, .. } = &mut wrapped.plan {
        forward.settles = Some(executed.image.clone());
        forward.image = launcher.clone();
        forward.program = String::from_utf8(launcher.path.clone()).unwrap();
    }
    assert_eq!(
        receipt.exec_candidate(Some(&executed), &wrapped),
        Some(true),
        "a wrapper must retain the exact attached process's settled identity"
    );
    assert_eq!(receipt.exec_candidate(Some(&attached), &wrapped), None);
    let mut wrapped_to_launcher = wrapped.clone();
    if let NativePlan::Restart { forward, .. } = &mut wrapped_to_launcher.plan {
        forward.settles = Some(launcher.clone());
    }
    assert_eq!(
        receipt.exec_candidate(Some(&attached), &wrapped_to_launcher),
        None
    );
    for changed_field in 0..8 {
        let mut changed = executed.clone();
        match changed_field {
            0 => changed.pid += 1,
            1 => changed.birth += 1,
            2 => changed.parent += 1,
            3 => changed.group += 1,
            4 => changed.session += 1,
            5 => changed.image.device += 1,
            6 => changed.image.inode += 1,
            _ => changed.image.path.push(b'x'),
        }
        assert_eq!(receipt.exec_candidate(Some(&changed), &wrapped), None);
    }
    for changed_field in 0..3 {
        let mut changed = receipt.clone();
        let candidate = changed.candidate.as_mut().unwrap();
        match changed_field {
            0 => candidate.launcher_device += 1,
            1 => candidate.launcher_inode += 1,
            _ => candidate.launcher_path.push(b'x'),
        }
        assert_eq!(changed.exec_candidate(Some(&executed), &wrapped), None);
    }
    // The next restart needs an independently completed native lineage,
    // not merely a fresh PID using this same thread and auth path.
    let mut original = prepare.clone();
    if let NativePlan::Restart { forward, .. } = &mut original.plan {
        forward.env = vec![("CODEX_AUTH_PATH".into(), "/new/auth.json".into())];
    }
    receipt.native_prepare = super::super::native_act::Request::prepare(original.clone())
        .encode()
        .unwrap();
    receipt.successor_pid = Some(executed.pid);
    receipt.successor_birth = Some(executed.birth);
    let mut completed = original.clone();
    if let NativePlan::Restart {
        process, forward, ..
    } = &mut completed.plan
    {
        *process = executed.clone();
        forward.image = Image {
            path: b"/codex-next".to_vec(),
            device: 1,
            inode: 3,
        };
        forward.program = "/codex-next".into();
    }
    let mut descendant = executed.clone();
    descendant.pid += 1;
    descendant.birth += 1;
    descendant.group = descendant.pid;
    if let NativePlan::Restart { forward, .. } = &completed.plan {
        descendant.image = forward.image.clone();
    }
    let registration = super::super::native_observe::Registration {
        id: "ef".repeat(16),
        host_epoch: completed.host_epoch.clone(),
        vendor: descendant.clone(),
        shell: shell.clone(),
    };
    let observed = super::super::native_observe::SavedObservation {
        binding: super::super::native_observe::ObservationBinding {
            id: registration.id.clone(),
            host_epoch: registration.host_epoch.clone(),
            capability: "ab".repeat(32),
        },
        registration,
    };
    assert!(receipt.matches_descendant(&completed, &observed, &descendant, true));
    assert!(!receipt.matches_descendant(&completed, &observed, &descendant, false));
    // Either generation may use a wrapper. Both lineage ends still name
    // the exact provider image observed after exec, never the wrapper.
    let wrap = |prepare: &mut Prepare| {
        let NativePlan::Restart { forward, .. } = &mut prepare.plan else {
            unreachable!()
        };
        forward.settles = Some(forward.image.clone());
        forward.image = launcher.clone();
        forward.program = String::from_utf8(launcher.path.clone()).unwrap();
    };
    for previous_wrapped in [false, true] {
        let mut previous = original.clone();
        if previous_wrapped {
            wrap(&mut previous);
        }
        receipt.native_prepare = super::super::native_act::Request::prepare(previous)
            .encode()
            .unwrap();
        for current_wrapped in [false, true] {
            let mut current = completed.clone();
            if current_wrapped {
                wrap(&mut current);
            }
            assert!(receipt.matches_descendant(&current, &observed, &descendant, true));
            if current_wrapped {
                let mut unfinished = descendant.clone();
                unfinished.image = launcher.clone();
                let mut premature = observed.clone();
                premature.registration.vendor = unfinished.clone();
                assert!(!receipt.matches_descendant(&current, &premature, &unfinished, true));
            }
            if previous_wrapped {
                let NativePlan::Restart { process, .. } = &mut current.plan else {
                    unreachable!()
                };
                process.image = launcher.clone();
                assert!(!receipt.matches_descendant(&current, &observed, &descendant, true));
            }
        }
    }
    let mut back = completed.clone();
    if let NativePlan::Restart {
        forward, rollback, ..
    } = &mut back.plan
    {
        *rollback = Some(forward.clone());
        rollback.as_mut().unwrap().image = executed.image.clone();
        rollback.as_mut().unwrap().program = "/codex".into();
    }
    let mut restored = observed.clone();
    restored.registration.vendor.image = executed.image.clone();
    let mut restored_prefix = descendant.clone();
    restored_prefix.image = launcher.clone();
    let mut retired = receipt.clone();
    retired.phase = Phase::Retired;
    retired.launch_attempted = true;
    retired.accepted_turn = None;
    retired.successor_pid = Some(descendant.pid);
    retired.successor_birth = Some(descendant.birth);
    retired.candidate = Some(Candidate::of(&restored_prefix, false));
    assert!(retired.matches_completed_rollback(&back, &restored));
    assert!(!retired.matches_completed_rollback(&back, &observed));
    let mut wrapped_back = back.clone();
    if let NativePlan::Restart { rollback, .. } = &mut wrapped_back.plan {
        let rollback = rollback.as_mut().unwrap();
        rollback.settles = Some(rollback.image.clone());
        rollback.image = launcher.clone();
        rollback.program = String::from_utf8(launcher.path.clone()).unwrap();
    }
    assert!(retired.matches_completed_rollback(&wrapped_back, &restored));
    assert!(!retired.matches_completed_rollback(&wrapped_back, &observed));
    let mut premature = restored.clone();
    premature.registration.vendor.image = launcher.clone();
    assert!(!retired.matches_completed_rollback(&wrapped_back, &premature));
    for change in 0..4 {
        let mut invalid = retired.clone();
        match change {
            0 => invalid.successor_birth = Some(descendant.birth + 1),
            1 => invalid.candidate.as_mut().unwrap().forward = true,
            2 => invalid.accepted_turn = Some(invalid.intent.wall.clone()),
            _ => invalid.phase = Phase::Committing,
        }
        assert!(!invalid.matches_completed_rollback(&back, &restored));
    }
    let mut forged = descendant.clone();
    forged.birth += 1;
    assert!(!receipt.matches_descendant(&completed, &observed, &forged, true));
    let mut wrong = completed.clone();
    if let NativePlan::Restart { process, .. } = &mut wrong.plan {
        process.birth += 1;
    }
    assert!(!receipt.matches_descendant(&wrong, &observed, &descendant, true));
    let mut wrong = completed.clone();
    if let NativePlan::Restart { forward, .. } = &mut wrong.plan {
        forward.env.clear();
    }
    assert!(!receipt.matches_descendant(&wrong, &observed, &descendant, true));
    executed.birth += 1;
    assert_eq!(receipt.exec_candidate(Some(&executed), &prepare), None);
    executed.birth -= 1;
    executed.image.inode += 1;
    assert_eq!(receipt.exec_candidate(Some(&executed), &prepare), None);
    executed.image.inode -= 1;
    receipt.candidate.as_mut().unwrap().launcher_inode += 1;
    assert_eq!(receipt.exec_candidate(Some(&executed), &prepare), None);
    receipt.candidate.as_mut().unwrap().launcher_inode -= 1;
    if let NativePlan::Restart { forward, .. } = &mut prepare.plan {
        forward.image = launcher.clone();
    }
    executed.image = launcher;
    assert_eq!(receipt.exec_candidate(Some(&executed), &prepare), None);
}

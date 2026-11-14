// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::session_timeline::scoped_input_tests::{install, scoped};
use std::sync::atomic::AtomicBool;

fn seal(ctx: &Arc<crate::SessionCtx>) -> Seal {
    let proof = scoped(&[Scope::Relaunch]).proven;
    let image = Image {
        path: b"/fixture-image".to_vec(),
        device: 1,
        inode: 2,
    };
    let process = Process {
        pid: 4242,
        birth: 12,
        parent: 42,
        group: 4242,
        session: 42,
        image: image.clone(),
    };
    let command = RestartCommand {
        settles: None,
        account_intent: None,
        image: image.clone(),
        // A path this host reads as absolute, as the plan's encoding requires, as is the
        // socket's (the image's path is the kernel's own bytes, POSIX on every host).
        program: crate::test_host::abs("/fixture-image"),
        argv: vec!["resume".into()],
        env: vec![("CODEX_AUTH_PATH".into(), "/new/auth.json".into())],
        unset: None,
        cwd: None,
        heal: None,
        dialect: aterm_agent::harness::upgrade::Dialect::Zsh,
    };
    let revision = ctx
        .meta
        .lock()
        .unwrap()
        .supervisor_authority(crate::metrics::now_us())
        .revision();
    let prepare = Prepare {
        receipt_mode: None,
        rejection_domain: None,
        asked: None,
        id: "ab".repeat(16),
        host_epoch: host_epoch().unwrap().into(),
        holder: crate::harness_host::holder(),
        revision,
        expected_human_seq: Some(ctx.human_input.seq()),
        deadline_us: u64::MAX,
        duty: Scope::Relaunch,
        plan: NativePlan::Restart {
            cold: None,
            process,
            shell: Process {
                pid: 42,
                birth: 10,
                parent: 1,
                group: 42,
                session: 42,
                image: image.clone(),
            },
            observation: None,
            launcher: image,
            socket: crate::test_host::abs("/fixture.sock"),
            forward: Box::new(command),
            rollback: None,
        },
        boot: None,
        custody: None,
    };
    let receipt = Receipt {
        destination: None,
        id: prepare.id.clone(),
        intent: Intent {
            purpose: Default::default(),
            thread: "019a4b54-5fa1-7123-8123-123456789abc".into(),
            wall: "019a4b54-5fa2-7123-8123-123456789abc".into(),
            identity: renewal::Identity::from_native("account", "user"),
        },
        sid: ctx.self_id.as_str().into(),
        old_pid: 4242,
        old_birth: 12,
        claim_revision: revision,
        human_seq: ctx.human_input.seq(),
        old_auth: "/old/auth.json".into(),
        new_auth: "/new/auth.json".into(),
        generation: 1,
        row_stamp: String::new(),
        native_prepare: String::new(),
        expires_ms: u64::MAX,
        phase: Phase::Ready,
        successor_pid: None,
        successor_birth: None,
        accepted_turn: None,
        launch_attempted: false,
        candidate: None,
        current_attempt: None,
        revoked: false,
    };
    Seal {
        receipt,
        prepare,
        proof,
        ctx: Arc::downgrade(ctx),
        env: Env::from_process().unwrap(),
        intent: Arc::new(IntentFence::new(ctx)),
        record: None,
        source: None,
        continuation: None,
        fresh_snapshot: None,
    }
}

#[test]
fn codex_renewal_service_recovers_a_late_root_before_host_work() {
    let handle = crate::session_store::test_handle_on(19101, -1);
    let ctx = Arc::clone(&handle.ctx);
    install(&ctx, 71, &[Scope::Relaunch]);
    let directory = aterm_tempfile::tempdir().unwrap();
    let mut seed = seal(&ctx);
    seed.env.home = Some(directory.path().join("home"));
    seed.env.state = directory.path().join("state");
    seed.prepare.deadline_us = 1;
    seed.receipt.native_prepare =
        aterm_agent::harness::native_act::Request::prepare(seed.prepare.clone())
            .encode()
            .unwrap();
    seed.receipt.phase = Phase::Committing;
    seed.receipt.launch_attempted = true;
    let env = &seed.env;
    let root = &seed.receipt;
    let options = opts(env, &root.sid).unwrap();
    renewal::with_receipt_writer(env, |_| renewal::write_receipt(env, root))
        .unwrap()
        .unwrap();
    assert!(service::root_allows_native_work(
        env,
        root,
        &seed.prepare,
        &options
    ));

    let native_dir = env.state.join("upgrade");
    aterm_types::fs_restricted::ensure_private_dir(&native_dir).unwrap();
    let file = format!("renewal-{}.json", seed.prepare.id);
    // A completed header without the positive NoExec fact is pending,
    // never permission to ask the host for another operation.
    let mut retained = aterm_json::json!({
        "phase": "done", "agent": "codex", "tab": root.sid,
        "native_coordination": true,
        "native_completed_prepare": root.native_prepare,
    });
    aterm_agent::harness::atomic_write::write_atomic_mode(
        &native_dir.join(&file),
        &retained.to_string(),
        Some(0o600),
    )
    .unwrap();
    assert!(!service::root_allows_native_work(
        env,
        root,
        &seed.prepare,
        &options
    ));
    assert_eq!(renewal::read_receipt(env, &root.id).as_ref(), Some(root));

    let fields = retained.as_object_mut().unwrap();
    let _completed = fields.remove("native_completed_prepare");
    let _noexec = fields.insert(
        "native_noexec_prepare".into(),
        aterm_json::Value::from(root.native_prepare.clone()),
    );
    aterm_agent::harness::atomic_write::write_atomic_mode(
        &native_dir.join(&file),
        &retained.to_string(),
        Some(0o600),
    )
    .unwrap();
    let failure = aterm_durable::seam::FailingDirs::after(0, aterm_durable::seam::EIO);
    assert!(!service::root_allows_native_work(
        env,
        root,
        &seed.prepare,
        &options
    ));
    assert_eq!(renewal::read_receipt(env, &root.id).as_ref(), Some(root));
    drop(failure);

    assert!(!service::root_allows_native_work(
        env,
        root,
        &seed.prepare,
        &options
    ));
    let retired = renewal::read_receipt(env, &root.id).unwrap();
    assert_eq!(retired.phase, Phase::Retired);
    assert!(!retired.launch_attempted);
    assert!(!service::root_allows_native_work(
        env,
        root,
        &seed.prepare,
        &options
    ));
    assert_eq!(renewal::read_receipt(env, &root.id), Some(retired));
}

#[test]
fn codex_renewal_terminal_rollback_keeps_service_until_completion_is_persisted() {
    let handle = crate::session_store::test_handle_on(8893, -1);
    let ctx = Arc::clone(&handle.ctx);
    install(&ctx, 71, &[Scope::Relaunch]);
    let directory = aterm_tempfile::tempdir().unwrap();
    let mut seed = seal(&ctx);
    seed.env.home = Some(directory.path().join("home"));
    seed.env.state = directory.path().join("state");
    seed.env.config = Some(directory.path().join("aterm.toml"));
    seed.receipt.intent.purpose = renewal::Purpose::WeeklyQuota;
    seed.receipt.destination = Some(renewal::Identity::from_native("target", "user"));
    seed.receipt.phase = Phase::Retired;
    seed.receipt.launch_attempted = true;
    let NativePlan::Restart { process, .. } = &seed.prepare.plan else {
        panic!("restart")
    };
    seed.receipt.candidate = Some(renewal::Candidate::of(process, false));
    seed.receipt.successor_pid = Some(process.pid);
    seed.receipt.successor_birth = Some(process.birth);
    seed.receipt.native_prepare =
        aterm_agent::harness::native_act::Request::prepare(seed.prepare.clone())
            .encode()
            .unwrap();
    let id = seed.receipt.id.clone();
    renewal::with_receipt_writer(&seed.env, |_| {
        renewal::write_receipt(&seed.env, &seed.receipt)
    })
    .unwrap();
    ctx.fenced_writes
        .native
        .lock()
        .unwrap()
        .renewal_services
        .insert(id.clone(), Arc::new(Service::new(seed)));
    assert!(!service::observe_terminal(&ctx, &id));
    assert!(
        ctx.fenced_writes
            .native
            .lock()
            .unwrap()
            .renewal_services
            .contains_key(&id)
    );
}

#[test]
fn codex_renewal_terminal_receipt_waits_for_directory_barrier() {
    for (offset, purpose) in [renewal::Purpose::Login, renewal::Purpose::WeeklyQuota]
        .into_iter()
        .enumerate()
    {
        let handle = crate::session_store::test_handle_on(8895 + offset as u64, -1);
        let ctx = Arc::clone(&handle.ctx);
        install(&ctx, 71, &[Scope::Relaunch]);
        let directory = aterm_tempfile::tempdir().unwrap();
        let mut seed = seal(&ctx);
        seed.env.home = Some(directory.path().join("home"));
        seed.env.state = directory.path().join("state");
        seed.env.config = Some(directory.path().join("aterm.toml"));
        seed.receipt.intent.purpose = purpose;
        if purpose == renewal::Purpose::WeeklyQuota {
            seed.receipt.destination = Some(renewal::Identity::from_native("target", "user"));
        }
        seed.receipt.phase = Phase::Committing;
        seed.receipt.launch_attempted = true;
        let env = seed.env.clone();
        let id = seed.receipt.id.clone();
        renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &seed.receipt))
            .unwrap()
            .unwrap();
        let mut completed = seed.receipt.clone();
        completed.phase = Phase::Committed;
        completed.successor_pid = Some(4243);
        completed.successor_birth = Some(13);
        ctx.fenced_writes
            .native
            .lock()
            .unwrap()
            .renewal_services
            .insert(id.clone(), Arc::new(Service::new(seed)));
        // The account guard is already held when the failure is armed:
        // permit the grandparent flush, then fail after the receipt rename.
        let failure = renewal::with_receipt_writer(&env, |_| {
            let failure = aterm_durable::seam::FailingDirs::after(1, aterm_durable::seam::EIO);
            assert_eq!(
                renewal::write_receipt(&env, &completed),
                Err("receipt-directory-flush")
            );
            Ok(failure)
        })
        .unwrap()
        .unwrap();
        assert_eq!(renewal::read_receipt(&env, &id), Some(completed));
        assert!(!service::observe_terminal(&ctx, &id));
        assert!(
            ctx.fenced_writes
                .native
                .lock()
                .unwrap()
                .renewal_services
                .contains_key(&id)
        );
        drop(failure);
        assert!(service::observe_terminal(&ctx, &id));
        assert!(
            !ctx.fenced_writes
                .native
                .lock()
                .unwrap()
                .renewal_services
                .contains_key(&id)
        );
    }
}

#[test]
fn codex_renewal_consumed_reply_repairs_the_barrier_and_rejects_a_stale_receipt() {
    let handle = crate::session_store::test_handle_on(8897, -1);
    let ctx = Arc::clone(&handle.ctx);
    install(&ctx, 71, &[Scope::Relaunch]);
    let directory = aterm_tempfile::tempdir().unwrap();
    let mut seed = seal(&ctx);
    seed.env.home = Some(directory.path().join("home"));
    seed.env.state = directory.path().join("state");
    seed.env.config = Some(directory.path().join("aterm.toml"));
    let env = seed.env;
    let mut consumed = seed.receipt;
    consumed.phase = Phase::Consumed;
    consumed.accepted_turn = Some(consumed.intent.wall.clone());
    let failure = renewal::with_receipt_writer(&env, |_| {
        let failure = aterm_durable::seam::FailingDirs::after(1, aterm_durable::seam::EIO);
        assert_eq!(
            renewal::write_receipt(&env, &consumed),
            Err("receipt-directory-flush")
        );
        Ok(failure)
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        acknowledged_receipt_reply(&env, &consumed),
        "ERR codex-renewal record unavailable\n"
    );
    drop(failure);
    assert!(acknowledged_receipt_reply(&env, &consumed).contains("state=consumed"));
    let mut newer = consumed.clone();
    newer.successor_pid = Some(99);
    newer.successor_birth = Some(100);
    renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &newer))
        .unwrap()
        .unwrap();
    assert_eq!(
        acknowledged_receipt_reply(&env, &consumed),
        "ERR codex-renewal record unavailable\n"
    );
    assert_eq!(renewal::read_receipt(&env, &newer.id), Some(newer));
}

/// A rotation that ended because its successor never attached took the pool's canary onto its
/// destination; its service gives that back only under the host's closure of its last budget,
/// never on the receipt alone. With no retained closure it keeps its service and retries.
#[test]
fn codex_rotation_ended_unattached_keeps_its_service_until_the_host_proves_closure() {
    // Its last budget's launcher may have attached, and the root ends unattached all the same.
    for (session, attached) in [(8898, false), (8899, true)] {
        ended_unattached_keeps_its_service(session, attached);
    }
}

fn ended_unattached_keeps_its_service(session: u64, attached: bool) {
    let handle = crate::session_store::test_handle_on(session, -1);
    let ctx = Arc::clone(&handle.ctx);
    install(&ctx, 71, &[Scope::Relaunch]);
    let directory = aterm_tempfile::tempdir().unwrap();
    let mut seed = seal(&ctx);
    seed.env.home = Some(directory.path().join("home"));
    seed.env.state = directory.path().join("state");
    seed.env.config = Some(directory.path().join("aterm.toml"));
    seed.receipt.intent.purpose = renewal::Purpose::WeeklyQuota;
    seed.receipt.destination = Some(renewal::Identity::from_native("target", "user"));
    seed.receipt.phase = Phase::Committing;
    seed.receipt.launch_attempted = true;
    seed.receipt.current_attempt = Some(renewal::AttemptRef {
        id: "cd".repeat(16),
        generation: service::MAX_CONTINUATIONS,
    });
    seed.receipt.native_prepare =
        aterm_agent::harness::native_act::Request::prepare(seed.prepare.clone())
            .encode()
            .unwrap();
    if attached {
        let NativePlan::Restart { shell, .. } = &seed.prepare.plan else {
            unreachable!("a restart recipe");
        };
        seed.receipt.candidate = Some(renewal::Candidate::of(shell, true));
    }
    let id = seed.receipt.id.clone();
    let env = seed.env.clone();
    renewal::with_receipt_writer(&env, |writer| {
        renewal::write_receipt(&env, &seed.receipt)?;
        writer.cancel(
            &seed.receipt,
            renewal::CancellationKind::SuccessorNeverAttached,
            0,
            0,
        )
    })
    .unwrap()
    .unwrap();
    ctx.fenced_writes
        .native
        .lock()
        .unwrap()
        .renewal_services
        .insert(id.clone(), Arc::new(Service::new(seed)));
    assert!(!service::observe_terminal(&ctx, &id));
    assert!(
        ctx.fenced_writes
            .native
            .lock()
            .unwrap()
            .renewal_services
            .contains_key(&id)
    );
}

/// A root that ended with no successor hands its session back on the login it was on, and the
/// agent that relaunch starts (a new pid, the same conversation, its record still bound to the
/// receipt it inherited) asks about that receipt. It is told the receipt retired, so it unbinds
/// and asks afresh, for a login renewal and a rotation alike; were it told it is the wrong
/// process, it would keep the point and ask again until a person typed. A process on the other
/// login, on another conversation, or whose record no longer names the receipt is still refused.
#[test]
fn codex_renewal_ended_root_answers_the_agent_handed_back_on_its_login() {
    for (session, purpose) in [
        (8903, renewal::Purpose::Login),
        (8904, renewal::Purpose::WeeklyQuota),
    ] {
        ended_root_answers_its_lineage(session, purpose);
    }
}

fn ended_root_answers_its_lineage(session: u64, purpose: renewal::Purpose) {
    let handle = crate::session_store::test_handle_on(session, -1);
    let ctx = Arc::clone(&handle.ctx);
    install(&ctx, 71, &[Scope::Relaunch]);
    let directory = aterm_tempfile::tempdir().unwrap();
    let mut seed = seal(&ctx);
    seed.env.home = Some(directory.path().join("home"));
    seed.env.state = directory.path().join("state");
    seed.env.config = Some(directory.path().join("aterm.toml"));
    seed.receipt.intent.purpose = purpose;
    if purpose == renewal::Purpose::WeeklyQuota {
        seed.receipt.destination = Some(renewal::Identity::from_native("target", "user"));
    }
    seed.receipt.phase = Phase::Committing;
    seed.receipt.launch_attempted = true;
    seed.receipt.current_attempt = Some(renewal::AttemptRef {
        id: "cd".repeat(16),
        generation: service::MAX_CONTINUATIONS,
    });
    seed.receipt.native_prepare =
        aterm_agent::harness::native_act::Request::prepare(seed.prepare.clone())
            .encode()
            .unwrap();
    let id = seed.receipt.id.clone();
    let env = seed.env.clone();
    renewal::with_receipt_writer(&env, |writer| {
        renewal::write_receipt(&env, &seed.receipt)?;
        writer.cancel(
            &seed.receipt,
            renewal::CancellationKind::SuccessorNeverAttached,
            0,
            0,
        )
    })
    .unwrap()
    .unwrap();
    let ended = renewal::read_receipt(&env, &id).unwrap();
    assert_eq!(ended.phase, Phase::Retired);
    assert!(ended.revoked && ended.successor_pid.is_none());
    // The relaunched agent: this test process, which is not the source the Signal ended.
    let pid = std::process::id();
    let proof = crate::self_claim::Proven {
        pid,
        start: aterm_agent::harness::restart_stamp::kernel_start(pid).unwrap(),
    };
    let home = directory.path().join("home/.codex");
    let copy = |thread: &str, auth: &str| self_claimed::Copy {
        lifetime: None,
        pid,
        start: proof.start.clone(),
        home: home.clone(),
        seen: self_claimed::Seen::Never,
        resume: Ok(self_claimed::Resume {
            argv: vec!["/fixture/codex-alab".into(), "resume".into(), thread.into()],
            session: thread.into(),
            codex_home: Some(home.to_string_lossy().into_owned()),
            codex_auth_path: Some(auth.into()),
            cwd: None,
        }),
    };
    let bound = |receipt: &str| match purpose {
        renewal::Purpose::Login => aterm_json::json!({"owed": {"renewal": {"receipt": receipt}}}),
        renewal::Purpose::WeeklyQuota => {
            aterm_json::json!({"owed": {"rotation": {"receipt": receipt}}})
        }
    };
    let ask = |copy: &self_claimed::Copy, record: &aterm_json::Value| {
        status(
            &ctx,
            &env,
            &proof,
            copy,
            record,
            &id,
            renewal::Request::Status(id.clone()),
        )
    };
    let thread = ended.intent.thread.clone();
    let lineage = copy(&thread, "/old/auth.json");
    let reply = ask(&lineage, &bound(&id));
    assert!(
        reply.starts_with("OK codex-") && reply.contains(" state=retired "),
        "{purpose:?}: {reply}"
    );
    // `serve_rotation` renames the rotation lane's prefix; `status` answers in this one.
    let wrong = "ERR codex-renewal wrong process\n";
    for (copy, record) in [
        (copy(&thread, "/new/auth.json"), bound(&id)),
        (
            copy("019a4b54-5fa9-7123-8123-123456789abc", "/old/auth.json"),
            bound(&id),
        ),
        (lineage.clone(), bound(&"ef".repeat(16))),
    ] {
        assert_eq!(ask(&copy, &record), wrong, "{purpose:?}: {copy:?} {record}");
    }
}

#[test]
fn codex_renewal_unknown_terminal_operation_keeps_launch_uncertainty() {
    let handle = crate::session_store::test_handle_on(8894, -1);
    let ctx = Arc::clone(&handle.ctx);
    install(&ctx, 71, &[Scope::Relaunch]);
    let directory = aterm_tempfile::tempdir().unwrap();
    let mut seed = seal(&ctx);
    seed.env.home = Some(directory.path().join("home"));
    seed.env.state = directory.path().join("state");
    seed.env.config = Some(directory.path().join("aterm.toml"));
    seed.receipt.intent.purpose = renewal::Purpose::WeeklyQuota;
    seed.receipt.destination = Some(renewal::Identity::from_native("target", "user"));
    seed.receipt.phase = Phase::Retired;
    seed.receipt.launch_attempted = true;
    seed.receipt.revoked = true;
    seed.receipt.native_prepare =
        aterm_agent::harness::native_act::Request::prepare(seed.prepare.clone())
            .encode()
            .unwrap();
    let id = seed.receipt.id.clone();
    let env = seed.env.clone();
    renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &seed.receipt)).unwrap();
    ctx.fenced_writes
        .native
        .lock()
        .unwrap()
        .renewal_services
        .insert(id.clone(), Arc::new(Service::new(seed)));
    assert!(!service::observe_terminal(&ctx, &id));
    assert!(renewal::read_receipt(&env, &id).unwrap().launch_attempted);
}

#[test]
fn codex_renewal_seal_is_exact_plan_claim_and_private_constructor_only() {
    let handle = crate::session_store::test_handle_on(8873, -1);
    let ctx = Arc::clone(&handle.ctx);
    install(&ctx, 71, &[Scope::Relaunch]);
    let seal = seal(&ctx);
    let meta = ctx.meta.lock().unwrap();
    let snapshot = meta.supervisor_authority(crate::metrics::now_us());
    assert!(seal.admits(&snapshot, &seal.prepare));
    let mut altered = seal.prepare.clone();
    altered.id = "cd".repeat(16);
    assert!(!seal.admits(&snapshot, &altered));
    if let NativePlan::Restart { forward, .. } = &mut altered.plan {
        forward.env.clear();
    }
    altered.id = seal.prepare.id.clone();
    assert!(!seal.admits(&snapshot, &altered));
    let actor = Process {
        pid: 900,
        birth: 1,
        parent: 1,
        group: 900,
        session: 900,
        image: Image {
            path: b"/host".to_vec(),
            device: 1,
            inode: 1,
        },
    };
    let external = SupervisorInputCaller {
        principal: crate::control::Principal::Instance,
        connection: Some(71),
        peer: Some(4242),
        proof: Some(seal.proof.clone()),
    };
    assert!(
        capture_native_authority(
            &ctx,
            &meta,
            &seal.prepare,
            &external,
            Some(actor.clone()),
            Some(Conversation::Facts(Arc::new(AtomicBool::new(true)))),
            crate::metrics::now_us(),
            Some(seal.clone()),
            None,
            &System(&ctx.native_policy, true),
        )
        .is_err()
    );
    let private = SupervisorInputCaller {
        principal: crate::control::Principal::Harness,
        connection: Some(72),
        peer: Some(900),
        proof: None,
    };
    assert!(
        capture_native_authority(
            &ctx,
            &meta,
            &seal.prepare,
            &private,
            Some(actor),
            Some(Conversation::Facts(Arc::new(AtomicBool::new(true)))),
            crate::metrics::now_us(),
            Some(seal.clone()),
            None,
            &System(&ctx.native_policy, true),
        )
        .is_ok()
    );
    drop(meta);
    install(&ctx, 73, &[Scope::Upgrade]);
    let changed = ctx
        .meta
        .lock()
        .unwrap()
        .supervisor_authority(crate::metrics::now_us());
    assert!(!seal.admits(&changed, &seal.prepare));
}

#[test]
fn codex_renewal_retained_prepare_replays_after_source_proof_is_gone() {
    let handle = crate::session_store::test_handle_on(8875, -1);
    let ctx = Arc::clone(&handle.ctx);
    install(&ctx, 71, &[Scope::Relaunch]);
    let seal = seal(&ctx);
    assert!(!seal.preflight());
    let actor = Process::observe(std::process::id()).unwrap();
    let caller = SupervisorInputCaller {
        principal: crate::control::Principal::Harness,
        connection: Some(72),
        peer: Some(actor.pid),
        proof: None,
    };
    let authority = {
        let meta = ctx.meta.lock().unwrap();
        let mut cold_request = seal.prepare.clone();
        let NativePlan::Restart { cold, rollback, .. } = &mut cold_request.plan else {
            unreachable!();
        };
        *cold = Some(Box::new(aterm_agent::harness::native_act::ColdIntent {
            login: None,
            home: None,
            auth_observed: false,
            route: aterm_agent::harness::accounts::AuthRoute::Stored,
            conversation: Some("captured-conversation".into()),
            profile: None,
            mode: aterm_agent::harness::accounts::Mode::Wall,
            billing: aterm_agent::harness::native_act::ColdBilling::Keep,
        }));
        *rollback = None;
        assert_eq!(
            cold_request.plan.constructor(),
            aterm_agent::harness::native_act::Constructor::ColdRestart
        );
        assert!(
            capture_native_authority(
                &ctx,
                &meta,
                &cold_request,
                &caller,
                Some(actor.clone()),
                Some(Conversation::Facts(Arc::new(AtomicBool::new(true)))),
                crate::metrics::now_us(),
                Some(seal.clone()),
                None,
                &System(&ctx.native_policy, true),
            )
            .is_err(),
            "a real renewal seal cannot authorize a different cold plan"
        );
        capture_native_authority(
            &ctx,
            &meta,
            &seal.prepare,
            &caller,
            Some(actor),
            Some(Conversation::Facts(Arc::new(AtomicBool::new(true)))),
            crate::metrics::now_us(),
            Some(seal.clone()),
            None,
            &System(&ctx.native_policy, true),
        )
        .unwrap()
    };
    let binding = Binding {
        id: seal.prepare.id.clone(),
        capability: "ab".repeat(32),
    };
    let operation = Arc::new(Mutex::new(Operation {
        terminal_acknowledged: None,
        request: seal.prepare.clone(),
        binding: binding.clone(),
        authority,
        custody: None,
        phase: super::super::Phase::Started,
        preparation: super::super::Preparation::recorded_for_test(),
        grant_record: super::super::GrantRecord::None,
        signal_sent: true,
        signal_accepted: Some(true),
        resume_sent: false,
        resume_attempt: None,
        human_seq: ctx.human_input.seq(),
        channel: Some(72),
        channel_closed: false,
        claim_outcome: ClaimOutcome::Builtin,
        commit_version: None,
        completion_drain: None,
        resume_phase: None,
        children: [None, None],
        cold_facts: None,
        cold_resolving: false,
        resolved_cold: None,
        effect_started: Arc::new(AtomicBool::new(true)),
        clock: Clock::System,
        observation: None,
        outcome_slot: None,
        completed_observation: None,
        committed_image: None,
        source_death: None,
        closed_restart: None,
        resume_only: false,
    }));
    {
        let mut table = ctx.fenced_writes.native.lock().unwrap();
        table.renewal_seals.insert(binding.id.clone(), seal.clone());
        table.by_id.insert(binding.id.clone(), operation);
    }
    let reply = super::super::prepare(
        &ctx,
        -1,
        seal.prepare.clone(),
        caller.clone(),
        &System(&ctx.native_policy, true),
    );
    assert_eq!(reply, admitted(&binding));
    let mut altered = seal.prepare.clone();
    if let NativePlan::Restart { forward, .. } = &mut altered.plan {
        forward.env.clear();
    }
    assert_eq!(
        super::super::prepare(&ctx, -1, altered, caller, &System(&ctx.native_policy, true)),
        DENIED
    );
}

#[test]
fn codex_renewal_cold_host_holds_nonterminal_but_keeps_terminal_evidence() {
    let handle = crate::session_store::test_handle_on(8879, -1);
    let mut sealed = seal(&handle.ctx);
    sealed.prepare.host_epoch = "ef".repeat(16);
    sealed.receipt.native_prepare =
        aterm_agent::harness::native_act::Request::prepare(sealed.prepare)
            .encode()
            .unwrap();
    for phase in [Phase::Ready, Phase::Committing, Phase::Committed] {
        sealed.receipt.phase = phase;
        assert!(receipt_reply(&sealed.receipt).contains("state=waiting"));
    }
    sealed.receipt.phase = Phase::Consumed;
    assert!(receipt_reply(&sealed.receipt).contains("state=consumed"));
    sealed.receipt.phase = Phase::Retired;
    assert!(receipt_reply(&sealed.receipt).contains("state=retired"));
}

#[test]
fn codex_renewal_grace_uses_native_human_clock_not_sequence_alone() {
    let handle = crate::session_store::test_handle_on(8877, -1);
    assert!(human_grace_elapsed(&handle.ctx, 2, 10_000_000));
    handle
        .ctx
        .human_input
        .note_reaching(10_000_000, crate::human_input::Gesture::Typed, true);
    let sequence = handle.ctx.human_input.seq();
    assert!(!human_grace_elapsed(&handle.ctx, 2, 10_001_000));
    assert!(human_grace_elapsed(&handle.ctx, 2, 12_000_000));
    assert_eq!(handle.ctx.human_input.seq(), sequence);
}

#[test]
fn codex_renewal_requires_sealed_platform_authority() {
    let policy = crate::native_session_policy::SessionPolicy::default();
    for kind in [Constructor::OwnImage, Constructor::PrivateRestart] {
        assert!(
            !System(&policy, false).constructor_enabled(kind),
            "an unsealed request cannot use a generic native constructor"
        );
    }
    assert!(!System(&policy, true).constructor_enabled(Constructor::OwnImage));
    assert_eq!(
        System(&policy, true).constructor_enabled(Constructor::PrivateRestart),
        cfg!(all(target_os = "macos", target_arch = "aarch64"))
    );
    for sealed in [false, true] {
        assert_eq!(
            System(&policy, sealed).constructor_enabled(Constructor::ColdRestart),
            crate::native_session_policy::cold_restart_qualified(),
            "the cold constructor is independent of a Codex renewal seal"
        );
    }
}

#[cfg(unix)]
#[test]
fn codex_renewal_respects_the_fixture_constructor_gate() {
    let mut handle = crate::session_store::test_handle_on(8874, -1);
    let image = Image {
        path: b"/fixture".to_vec(),
        device: 1,
        inode: 1,
    };
    let policy = crate::native_session_policy::FixturePolicy::new(
        std::slice::from_ref(
            aterm_agent::harness::contract::row_named("grok-alab").expect("rostered"),
        ),
        vec![crate::native_session_policy::FixtureImages {
            row: "grok-alab",
            images: vec![image],
        }],
        aterm_agent::supervise::SupervisorConfig::default(),
        &[Constructor::PrivateRestart],
    )
    .unwrap();
    let gate = policy.constructors();
    assert!(
        Arc::get_mut(&mut handle.ctx)
            .unwrap()
            .install_fixture_policy(policy)
            .is_ok()
    );
    assert!(
        System(&handle.ctx.native_policy, true).constructor_enabled(Constructor::PrivateRestart)
    );
    gate.set_enabled(&[]);
    assert!(
        !System(&handle.ctx.native_policy, true).constructor_enabled(Constructor::PrivateRestart)
    );
}

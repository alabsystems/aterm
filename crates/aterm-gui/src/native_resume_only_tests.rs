// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::control_input_ack::{InputMark, testing};
use crate::input::{Delivery, EgressReceipt};
use aterm_agent::harness::native_receipts::testing::{Append, Journal, State as Stored};
use aterm_agent::harness::{accounts, cli::Env, codex_renewal as recovery};

#[path = "native_resume_phase_tests.rs"]
mod phase;

struct World {
    _root: aterm_tempfile::TempDir,
    handle: crate::session_store::SessionHandle,
    reader: std::fs::File,
    facts: Facts,
    actor: SupervisorInputCaller,
    env: Env,
    root: recovery::Receipt,
    original: Prepare,
    old: Binding,
    seal: renewal::Seal,
}

fn use_real_journal(world: &mut World) -> aterm_tempfile::TempDir {
    let home = aterm_tempfile::tempdir().unwrap();
    let journal = Journal::new(home.path());
    journal
        .record(&world.original, &world.old, Append::Prepare)
        .unwrap();
    journal
        .record(
            &world.original,
            &world.old,
            Append::NoExecInPhase(
                lookup(&world.handle.ctx, &world.old)
                    .unwrap()
                    .lock()
                    .unwrap()
                    .resume_phase
                    .unwrap_or(ImagePhase::Forward),
            ),
        )
        .unwrap();
    world.facts.native_journal = Some(journal);
    home
}

fn auth(home: &std::path::Path) {
    std::fs::create_dir_all(home).unwrap();
    let claims = aterm_json::json!({"https://api.openai.com/auth":{"chatgpt_account_id":"workspace","chatgpt_user_id":"user"}});
    let encoded = aterm_codec::base64::encode(claims.to_string().as_bytes())
        .unwrap()
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_string();
    std::fs::write(home.join("auth.json"),aterm_json::json!({"auth_mode":"chatgpt",
        "tokens":{"id_token":format!("e30.{encoded}.fixture"),"access_token":"fixture","refresh_token":"fixture"}}).to_string()).unwrap();
}

fn world(local: u64) -> World {
    world_with_retirement(local, true)
}

fn world_with_retirement(local: u64, retired: bool) -> World {
    world_with_signal(local, retired, true)
}

fn world_with_signal(local: u64, retired: bool, signal: bool) -> World {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let mut env = Env::from_process().unwrap();
    env.home = Some(tmp.path().join("home"));
    env.state = tmp.path().join("harness");
    env.config = Some(tmp.path().join("aterm.toml"));
    crate::test_scratch::write_config(
        env.config.as_ref().unwrap(),
        "[harness]\nhuman_grace_s=0\nlogins='renew'\n",
    );
    let old = tmp.path().join("auth-old");
    let new = tmp.path().join("auth-new");
    auth(&old);
    auth(&new);
    std::fs::write(env.accounts_roster_for(accounts::Cli::Codex).unwrap(),format!(
        "[[account]]\nlabel='main'\nclass='dedicated'\ndir={}\nemail='fixture@example.invalid'\n",
        aterm_toml::Value::from(old.to_str().unwrap()))).unwrap();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    std::fs::create_dir_all(&store.dir).unwrap();
    let mut logins = accounts::Logins {
        generation_clock: 2,
        ..Default::default()
    };
    logins.labels.insert(
        "main".into(),
        accounts::Binding {
            dir: Some(new.to_string_lossy().into_owned()),
            was: vec![Some(old.to_string_lossy().into_owned())],
            renewal_generation: 2,
            last_success_generation: 2,
            ..Default::default()
        },
    );
    std::fs::write(store.logins_file(), logins.to_json()).unwrap();
    let identity = recovery::Identity::from_native("workspace", "user");
    let selected = recovery::with_successor(&env, &old.join("auth.json"), &identity, |next| {
        Ok(next.clone())
    })
    .unwrap()
    .unwrap();
    let (handle, reader) = testing::owned_handle(local);
    let (handle, facts, mut original, actor) = super::closed_restart::fixture_on(handle);
    if let NativePlan::Restart {
        forward, rollback, ..
    } = &mut original.plan
    {
        forward.env = vec![(
            "CODEX_AUTH_PATH".into(),
            new.join("auth.json").to_string_lossy().into_owned(),
        )];
        rollback.as_mut().unwrap().env = vec![(
            "CODEX_AUTH_PATH".into(),
            old.join("auth.json").to_string_lossy().into_owned(),
        )];
    }
    let root = recovery::Receipt {
        destination: None,
        id: original.id.clone(),
        intent: recovery::Intent {
            purpose: Default::default(),
            thread: "019a4b54-5fa1-7123-8123-123456789abc".into(),
            wall: "019a4b54-5fa2-7123-8123-123456789abc".into(),
            identity,
        },
        sid: handle.ctx.self_id.as_str().into(),
        old_pid: 4242,
        old_birth: facts.process(4242).unwrap().birth,
        claim_revision: original.revision,
        human_seq: handle.ctx.human_input.seq(),
        old_auth: selected.old_auth,
        new_auth: selected.new_auth,
        generation: selected.generation,
        row_stamp: recovery::row_stamp(&selected.row),
        native_prepare: Request::prepare(original.clone()).encode().unwrap(),
        expires_ms: u64::MAX,
        phase: recovery::Phase::Committing,
        successor_pid: None,
        successor_birth: None,
        accepted_turn: None,
        launch_attempted: signal,
        candidate: None,
        current_attempt: None,
        revoked: false,
    };
    recovery::with_receipt_writer(&env, |_| recovery::write_receipt(&env, &root))
        .unwrap()
        .unwrap();
    let home = tmp.path().join("codex-home");
    let records = home.join("alab/sessions");
    std::fs::create_dir_all(&records).unwrap();
    std::fs::write(records.join("4242.json"),aterm_json::json!({"sessionId":root.intent.thread,"status":"waiting",
        "draftEmpty":true,"autopilot":"active","owed":{"thread":root.intent.thread,"mode":"land",
        "nativeOrigin":{"turnId":root.intent.wall,"phase":"complete"},"rule":"login-back@v1",
        "renewal":{"account":root.intent.identity.account,"user":root.intent.identity.user,"receipt":root.id,
        "openaiScope":true,"starts":1,"goalId":null}}}).to_string()).unwrap();
    let history = home.join("sessions/2026/11/03");
    std::fs::create_dir_all(&history).unwrap();
    let events = [
        aterm_json::json!({"type":"task_started","turn_id":root.intent.wall}),
        aterm_json::json!({"type":"error","codex_error_info":"unauthorized"}),
        aterm_json::json!({"type":"task_complete","turn_id":root.intent.wall}),
    ];
    let text = events
        .iter()
        .map(|payload| {
            format!(
                "{}\n",
                aterm_json::json!({"type":"event_msg","payload":payload})
            )
        })
        .collect::<String>();
    std::fs::write(
        history.join(format!("rollout-fixture-{}.jsonl", root.intent.thread)),
        text,
    )
    .unwrap();
    let old_binding = admitted_binding(&run(
        &handle.ctx,
        &facts,
        Request::prepare(original.clone()),
        actor.clone(),
    ));
    if signal {
        assert_eq!(
            run(
                &handle.ctx,
                &facts,
                Request::Signal(old_binding.clone()),
                actor.clone()
            ),
            "OK native-act state=signalled\n"
        );
    }
    // The unit kernel supplies the original authorized source; this injection
    // does not bypass any new dead-source admission or permit an actual effect.
    let operation = lookup(&handle.ctx, &old_binding).unwrap();
    let seal = renewal::fixture_after_signal(
        &handle.ctx,
        root.clone(),
        original.clone(),
        env.clone(),
        home,
    );
    operation.lock().unwrap().authority.renewal = Some(seal.clone());
    if signal {
        facts.processes.lock().unwrap().remove(&4242);
        facts.foreground.store(400, Ordering::SeqCst);
    }
    if retired {
        assert_eq!(
            run(
                &handle.ctx,
                &facts,
                Request::Reconcile(old_binding.clone()),
                actor.clone()
            ),
            "OK native-act state=retired reason=no-exec-grant\n"
        );
    }
    // The original built-in Relaunch was outside the external Upgrade claim.
    // Exercise fresh continuation authority against a distinct current claim.
    if retired {
        install(&handle.ctx, 71, &[Scope::Upgrade, Scope::Relaunch]);
    }
    facts.clock.store(
        if retired {
            original.deadline_us + 1
        } else {
            original.deadline_us - 1
        },
        Ordering::Release,
    );
    World {
        _root: tmp,
        handle,
        reader,
        facts,
        actor,
        env,
        root,
        original,
        old: old_binding,
        seal,
    }
}

fn publish(world: &World, id: &str) -> (recovery::Receipt, recovery::Attempt, Prepare) {
    let request = Prepare {
        asked: None,
        id: id.repeat(16),
        revision: world
            .handle
            .ctx
            .meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us())
            .revision(),
        expected_human_seq: Some(world.handle.ctx.human_input.seq()),
        deadline_us: world.facts.clock.load(Ordering::Acquire) + 120_000_000,
        ..world.original.clone()
    };
    recovery::with_receipt_writer(&world.env, |writer| {
        let attempt = writer.propose(&world.root, &world.original.id, &request)?;
        let root = writer.publish(&world.root, &attempt)?;
        Ok((root, attempt, request))
    })
    .unwrap()
    .unwrap()
}

#[test]
fn fresh_resume_only_admission_preserves_old_budget_and_never_resignals() {
    let mut world = world(7991);
    crate::app_input::note_person_reaching(
        &world.handle.ctx,
        crate::human_input::Gesture::Other,
        false,
    );
    let (root, attempt, request) = publish(&world, "bc");
    assert_eq!(
        renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts),
        Ok(request.clone())
    );
    let binding = admitted_binding(&run(
        &world.handle.ctx,
        &world.facts,
        Request::prepare(request.clone()),
        world.actor.clone(),
    ));
    assert_ne!(binding.id, world.old.id);
    assert_ne!(binding.capability, world.old.capability);
    {
        let journal = world.facts.journal.lock().unwrap();
        assert_eq!(journal[&world.old.id].successor.as_ref(), Some(&binding.id));
        assert_eq!(
            journal[&binding.id].predecessor.as_ref(),
            Some(&world.old.id)
        );
        assert_eq!(journal[&world.old.id].state, JournalState::NoExec);
    }
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::Signal(binding.clone()),
            world.actor.clone()
        ),
        DENIED
    );
    assert_eq!(world.facts.signals.load(Ordering::Acquire), 1);
    assert_eq!(
        world.facts.watch_count.load(Ordering::Acquire),
        1,
        "never arm against the dead PID"
    );
    assert_eq!(
        lookup(&world.handle.ctx, &world.old)
            .unwrap()
            .lock()
            .unwrap()
            .request,
        world.original
    );
    assert_eq!(root.native_prepare, world.root.native_prepare);
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request.clone()),
            world.actor.clone()
        ),
        admitted(&binding)
    );
    let ctx = &world.handle.ctx;
    let reply = serve_by(
        ctx,
        -1,
        Request::Resume(
            binding,
            ImagePhase::Forward,
            "1.1".into(),
            ctx.human_input.seq(),
        ),
        world.actor.clone(),
        &world.facts,
        &mut |_| {
            let mark = InputMark::at_epoch(
                ctx,
                world.handle.local_id,
                aterm_slog::Actor::Ctl,
                crate::session_log::InputVerb::Turn,
                None,
                true,
            );
            let producer = mark.native_producer();
            let completion = mark.completion();
            assert!(completion.start_supervised());
            let receipt = ctx.sink.write_frame_with_prefix(b"R").unwrap();
            assert!(
                ctx.sink
                    .wait_prefix_to_kernel(receipt.kernel_prefix().unwrap())
            );
            completion.finish(EgressReceipt::from_reported_delivery(
                Delivery::Full,
                receipt.order(),
            ));
            drop(producer);
            "OK typed=1\n".into()
        },
    );
    assert_eq!(reply, "OK native-act state=resumed phase=forward\n");
    assert_eq!(testing::byte(&mut world.reader), b'R');
}

#[test]
fn swept_noexec_is_persisted_before_claim_and_failure_preserves_the_slot() {
    let world = world(8027);
    // A GUI sweep established the same positive closure, but no control worker
    // has yet recorded its terminal fact. Disk still contains only Prepared.
    world
        .facts
        .journal
        .lock()
        .unwrap()
        .get_mut(&world.old.id)
        .unwrap()
        .state = JournalState::Prepared;
    world.facts.receipt_history.lock().unwrap().clear();
    let (root, attempt, request) = publish(&world, "cd");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    world.facts.receipt_allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request.clone()),
            world.actor.clone()
        ),
        UNKNOWN
    );
    assert!(
        lookup(&world.handle.ctx, &world.old)
            .unwrap()
            .lock()
            .unwrap()
            .closed_restart
            .as_ref()
            .unwrap()
            .available()
    );
    assert!(
        !world
            .facts
            .journal
            .lock()
            .unwrap()
            .contains_key(&request.id)
    );
    world.facts.receipt_allowed.store(true, Ordering::SeqCst);
    let binding = admitted_binding(&run(
        &world.handle.ctx,
        &world.facts,
        Request::prepare(request),
        world.actor.clone(),
    ));
    let journal = world.facts.journal.lock().unwrap();
    assert_eq!(journal[&world.old.id].state, JournalState::NoExec);
    assert_eq!(journal[&world.old.id].successor.as_ref(), Some(&binding.id));
}

#[test]
fn predecessor_publication_holds_no_native_or_gui_mutex() {
    let mut world = world(8028);
    let (root, attempt, request) = publish(&world, "ce");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    let (entered, receiver) = mpsc::sync_channel(1);
    let proceed = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    world.facts.receipt_gate = Some((entered, proceed.clone()));
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            run(
                &world.handle.ctx,
                &world.facts,
                Request::prepare(request),
                world.actor.clone(),
            )
        });
        receiver.recv_timeout(PATIENCE).unwrap();
        assert!(world.handle.ctx.meta.try_lock().is_ok());
        assert!(world.handle.ctx.fenced_writes.native.try_lock().is_ok());
        assert_eq!(
            world
                .handle
                .ctx
                .fenced_writes
                .native
                .lock()
                .unwrap()
                .receipt_flights
                .get(&world.old.id),
            Some(&1)
        );
        assert!(
            lookup(&world.handle.ctx, &world.old)
                .unwrap()
                .try_lock()
                .is_ok()
        );
        *proceed.0.lock().unwrap() = true;
        proceed.1.notify_all();
        admitted_binding(&worker.join().unwrap());
    });
}

#[test]
fn successor_claim_io_is_unlocked_and_failure_never_admits_a_child() {
    let mut world = world(8029);
    let (root, attempt, request) = publish(&world, "cf");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    let (entered, receiver) = mpsc::sync_channel(1);
    let proceed = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    world.facts.prepare_journal_gate = Some((entered, proceed.clone()));
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            run(
                &world.handle.ctx,
                &world.facts,
                Request::prepare(request.clone()),
                world.actor.clone(),
            )
        });
        receiver.recv_timeout(PATIENCE).unwrap();
        assert!(world.handle.ctx.meta.try_lock().is_ok());
        assert!(world.handle.ctx.fenced_writes.native.try_lock().is_ok());
        {
            let mut table = world.handle.ctx.fenced_writes.native.lock().unwrap();
            assert_eq!(table.receipt_flights.get(&world.old.id), Some(&1));
            assert_eq!(table.receipt_flights.get(&request.id), Some(&1));
            let retired = table.reclaim(world.facts.clock.load(Ordering::SeqCst));
            assert!(table.by_id.contains_key(&world.old.id));
            drop(table);
            drop(retired);
        }
        assert!(
            lookup(&world.handle.ctx, &world.old)
                .unwrap()
                .try_lock()
                .is_ok()
        );
        world
            .facts
            .prepare_journal_allowed
            .store(false, Ordering::SeqCst);
        *proceed.0.lock().unwrap() = true;
        proceed.1.notify_all();
        assert_eq!(worker.join().unwrap(), UNKNOWN);
    });
    assert!(
        !world
            .facts
            .journal
            .lock()
            .unwrap()
            .contains_key(&request.id)
    );
    assert!(
        world.facts.journal.lock().unwrap()[&world.old.id]
            .successor
            .is_none()
    );
    assert_eq!(world.facts.signals.load(Ordering::SeqCst), 1);
    assert_eq!(world.facts.grant_journal_calls.load(Ordering::SeqCst), 0);
    let operation =
        world.handle.ctx.fenced_writes.native.lock().unwrap().by_id[&request.id].clone();
    let binding = operation.lock().unwrap().binding.clone();
    assert_eq!(operation.lock().unwrap().phase, Phase::Queued);
    assert!(operation.lock().unwrap().custody.is_none());
    connection_closed_by(
        &world.handle.ctx,
        world.actor.connection.unwrap(),
        &world.facts,
    );
    world.facts.prepare_journal_gate = None;
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request.clone()),
            world.actor.clone()
        ),
        UNKNOWN
    );
    assert!(
        !lookup(&world.handle.ctx, &world.old)
            .unwrap()
            .lock()
            .unwrap()
            .closed_restart
            .as_ref()
            .unwrap()
            .available()
    );
    world
        .facts
        .prepare_journal_allowed
        .store(true, Ordering::SeqCst);
    let reconnected = SupervisorInputCaller {
        connection: Some(72),
        ..world.actor.clone()
    };
    assert_eq!(
        admitted_binding(&run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request),
            reconnected
        )),
        binding,
        "an exact retry confirms the already claimed candidate"
    );
}

#[test]
fn a_lost_successor_append_ack_retries_without_sealing_the_committed_child() {
    let mut world = world(8115);
    let _journal_home = use_real_journal(&mut world);
    let (root, attempt, request) = publish(&world, "d1");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    world
        .facts
        .prepare_journal_ack
        .store(false, Ordering::SeqCst);
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request.clone()),
            world.actor.clone()
        ),
        UNKNOWN
    );
    let binding = world.facts.journal.lock().unwrap()[&request.id]
        .binding
        .clone();
    assert_eq!(
        world.facts.journal.lock().unwrap()[&request.id].state,
        JournalState::Prepared
    );
    assert!(world.facts.sealed_history.lock().unwrap().is_empty());
    assert_eq!(
        world
            .facts
            .native_journal
            .as_ref()
            .unwrap()
            .state(&binding)
            .unwrap(),
        Some(Stored::Prepared)
    );
    let operation = lookup(&world.handle.ctx, &binding).unwrap();
    assert_eq!(operation.lock().unwrap().phase, Phase::Queued);
    assert!(operation.lock().unwrap().custody.is_none());
    connection_closed_by(
        &world.handle.ctx,
        world.actor.connection.unwrap(),
        &world.facts,
    );
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::Reconcile(binding.clone()),
            world.actor.clone()
        ),
        UNKNOWN
    );
    world
        .facts
        .prepare_journal_ack
        .store(true, Ordering::SeqCst);
    let reconnected = SupervisorInputCaller {
        connection: Some(72),
        ..world.actor.clone()
    };
    assert_eq!(
        admitted_binding(&run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request),
            reconnected
        )),
        binding
    );
    assert_eq!(
        world
            .facts
            .prepared_history
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, b)| b == &binding)
            .count(),
        1
    );
    assert_eq!(world.facts.signals.load(Ordering::SeqCst), 1);
    assert_eq!(world.facts.grant_journal_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn an_expired_successor_can_repair_its_append_but_cannot_gain_admission() {
    let mut world = world(8116);
    let _journal_home = use_real_journal(&mut world);
    let (root, attempt, request) = publish(&world, "d2");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    world
        .facts
        .prepare_journal_ack
        .store(false, Ordering::SeqCst);
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request.clone()),
            world.actor.clone()
        ),
        UNKNOWN
    );
    world
        .facts
        .prepare_journal_ack
        .store(true, Ordering::SeqCst);
    world
        .facts
        .clock
        .store(request.deadline_us + 1, Ordering::SeqCst);
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request.clone()),
            world.actor.clone()
        ),
        NOEXEC
    );
    let journal = world.facts.journal.lock().unwrap();
    assert_eq!(journal[&request.id].state, JournalState::NoExec);
    assert_eq!(
        world
            .facts
            .native_journal
            .as_ref()
            .unwrap()
            .state(&journal[&request.id].binding)
            .unwrap(),
        Some(Stored::NoExec)
    );
    assert_eq!(journal[&world.old.id].successor.as_ref(), Some(&request.id));
    assert_eq!(world.facts.signals.load(Ordering::SeqCst), 1);
    assert_eq!(world.facts.grant_journal_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_writer_panic_after_commit_releases_custody_and_preserves_retry() {
    let mut world = world(8117);
    let _journal_home = use_real_journal(&mut world);
    let (root, attempt, request) = publish(&world, "d3");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    world
        .facts
        .prepare_journal_panic
        .store(true, Ordering::SeqCst);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request.clone()),
            world.actor.clone()
        )))
        .is_err()
    );
    let operation =
        world.handle.ctx.fenced_writes.native.lock().unwrap().by_id[&request.id].clone();
    let binding = operation.lock().unwrap().binding.clone();
    assert_eq!(operation.lock().unwrap().phase, Phase::Queued);
    assert!(operation.lock().unwrap().custody.is_none());
    assert_eq!(
        world
            .facts
            .native_journal
            .as_ref()
            .unwrap()
            .state(&binding)
            .unwrap(),
        Some(Stored::Prepared)
    );
    world
        .facts
        .prepare_journal_panic
        .store(false, Ordering::SeqCst);
    assert_eq!(
        admitted_binding(&run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request),
            world.actor.clone()
        )),
        binding
    );
}

#[test]
fn an_abandoned_successor_waits_for_its_cohort_instead_of_sealing_opaque() {
    let mut world = world(8118);
    let _journal_home = use_real_journal(&mut world);
    let (root, attempt, request) = publish(&world, "d4");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    world
        .facts
        .prepare_journal_ack
        .store(false, Ordering::SeqCst);
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request.clone()),
            world.actor.clone()
        ),
        UNKNOWN
    );
    let binding = world.facts.journal.lock().unwrap()[&request.id]
        .binding
        .clone();
    world
        .facts
        .prepare_journal_ack
        .store(true, Ordering::SeqCst);
    world
        .facts
        .clock
        .store(request.deadline_us + 1, Ordering::SeqCst);
    world
        .facts
        .processes
        .lock()
        .unwrap()
        .insert(911, process(911, 1, 4242, 400, 9));
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::Reconcile(binding.clone()),
            world.actor.clone()
        ),
        UNKNOWN
    );
    assert_eq!(
        world
            .facts
            .native_journal
            .as_ref()
            .unwrap()
            .state(&binding)
            .unwrap(),
        Some(Stored::Prepared)
    );
    let operation = lookup(&world.handle.ctx, &binding).unwrap();
    assert_eq!(operation.lock().unwrap().phase, Phase::Queued);
    assert!(operation.lock().unwrap().custody.is_none());
    world.facts.processes.lock().unwrap().remove(&911);
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::Reconcile(binding.clone()),
            world.actor.clone()
        ),
        NOEXEC
    );
    assert_eq!(
        world
            .facts
            .native_journal
            .as_ref()
            .unwrap()
            .state(&binding)
            .unwrap(),
        Some(Stored::NoExec)
    );
}

#[test]
fn a_first_leg_root_retires_only_after_positive_native_no_start() {
    let world = world_with_signal(8121, false, false);
    assert!(!renewal::fixture_retire_unstarted(
        &world.handle.ctx,
        &world.env,
        &world.root
    ));
    assert_eq!(
        recovery::read_receipt(&world.env, &world.root.id).unwrap(),
        world.root
    );
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::Abort(world.old.clone()),
            world.actor.clone()
        ),
        ROLLBACK_SAFE
    );
    assert!(renewal::fixture_retire_unstarted(
        &world.handle.ctx,
        &world.env,
        &world.root
    ));
    let retired = recovery::read_receipt(&world.env, &world.root.id).unwrap();
    assert_eq!(retired.phase, recovery::Phase::Retired);
    assert!(!retired.launch_attempted);
    assert!(
        !renewal::fixture_retire_unstarted(&world.handle.ctx, &world.env, &world.root),
        "old root cannot replace a changed record"
    );
}

#[test]
fn native_no_start_does_not_erase_conflicting_execution_metadata() {
    let world = world_with_signal(8130, false, false);
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::Abort(world.old.clone()),
            world.actor.clone()
        ),
        ROLLBACK_SAFE
    );
    for case in 0..6 {
        let mut root = world.root.clone();
        match case {
            0 => root.launch_attempted = true,
            1 => root.accepted_turn = Some(root.intent.wall.clone()),
            2 => {
                root.current_attempt = Some(recovery::AttemptRef {
                    id: "b".repeat(32),
                    generation: 1,
                });
            }
            3 => {
                root.candidate = Some(recovery::Candidate {
                    pid: 10,
                    birth: 1,
                    parent: 1,
                    group: 10,
                    session: 10,
                    launcher_device: 1,
                    launcher_inode: 2,
                    launcher_path: b"/native/launcher".to_vec(),
                    forward: true,
                });
            }
            4 => root.successor_pid = Some(10),
            5 => root.successor_birth = Some(1),
            _ => unreachable!(),
        }
        recovery::write_receipt(&world.env, &root).unwrap();
        let path = recovery::receipt_path(&world.env, &root.id).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(
            !renewal::fixture_retire_unstarted(&world.handle.ctx, &world.env, &root),
            "execution witness {case} must survive even a native no-start proof"
        );
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}

#[test]
fn a_retry_captures_connection_loss_during_publication() {
    let mut world = world(8122);
    let (root, attempt, request) = publish(&world, "d5");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    world
        .facts
        .prepare_journal_allowed
        .store(false, Ordering::SeqCst);
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request.clone()),
            world.actor.clone()
        ),
        UNKNOWN
    );
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request.clone()),
            SupervisorInputCaller {
                connection: None,
                ..world.actor.clone()
            }
        ),
        DENIED
    );
    world
        .facts
        .prepare_journal_allowed
        .store(true, Ordering::SeqCst);
    let (entered, receiver) = mpsc::sync_channel(1);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    world.facts.prepare_journal_gate = Some((entered, Arc::clone(&gate)));
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            run(
                &world.handle.ctx,
                &world.facts,
                Request::prepare(request.clone()),
                SupervisorInputCaller {
                    connection: Some(72),
                    ..world.actor.clone()
                },
            )
        });
        receiver.recv_timeout(PATIENCE).unwrap();
        assert!(
            world
                .handle
                .ctx
                .fenced_writes
                .native
                .lock()
                .unwrap()
                .coordinated_connections
                .contains(&72)
        );
        connection_closed_by(&world.handle.ctx, 72, &world.facts);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert_eq!(worker.join().unwrap(), NOEXEC);
    });
    assert_eq!(world.facts.grant_journal_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn cancellation_keeps_settling_an_unacknowledged_native_journal() {
    let mut world = world(8123);
    let _journal_home = use_real_journal(&mut world);
    let (root, attempt, request) = publish(&world, "d6");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    world
        .facts
        .prepare_journal_ack
        .store(false, Ordering::SeqCst);
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request.clone()),
            world.actor.clone()
        ),
        UNKNOWN
    );
    let binding = world.facts.journal.lock().unwrap()[&request.id]
        .binding
        .clone();
    world.seal.cancel(recovery::CancellationKind::OwnerStop);
    assert!(!renewal::fixture_settle_terminal_native(
        &world.handle.ctx,
        &world.seal,
        &world.facts
    ));
    assert_eq!(
        world
            .facts
            .native_journal
            .as_ref()
            .unwrap()
            .state(&binding)
            .unwrap(),
        Some(Stored::Prepared)
    );
    world
        .facts
        .prepare_journal_ack
        .store(true, Ordering::SeqCst);
    assert!(renewal::fixture_settle_terminal_native(
        &world.handle.ctx,
        &world.seal,
        &world.facts
    ));
    assert_eq!(
        world
            .facts
            .native_journal
            .as_ref()
            .unwrap()
            .state(&binding)
            .unwrap(),
        Some(Stored::NoExec)
    );
    assert_eq!(
        lookup(&world.handle.ctx, &binding)
            .unwrap()
            .lock()
            .unwrap()
            .phase,
        Phase::Completed(Decision::Retired(Retirement::NotGranted))
    );
    assert_eq!(world.facts.grant_journal_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_reconcile_disconnect_does_not_cancel_a_valid_preparation() {
    use std::os::fd::AsRawFd;

    let (handle, facts, prepare, actor) = super::closed_restart::fixture(8124);
    facts.prepare_journal_ack.store(false, Ordering::SeqCst);
    assert_eq!(
        run(
            &handle.ctx,
            &facts,
            Request::prepare(prepare.clone()),
            actor.clone()
        ),
        UNKNOWN
    );
    let operation = handle.ctx.fenced_writes.native.lock().unwrap().by_id[&prepare.id].clone();
    let binding = operation.lock().unwrap().binding.clone();
    facts.prepare_journal_ack.store(true, Ordering::SeqCst);
    let (client, server) = aterm_uds::CtlStream::pair().unwrap();
    drop(client);
    aterm_spec::wait::until_peer_closed(&server);
    {
        let _serving = aterm_control::hangup::ServingFd::enter(server.as_raw_fd());
        assert!(aterm_control::hangup::caller_hung_up());
        assert_eq!(
            run(
                &handle.ctx,
                &facts,
                Request::Reconcile(binding.clone()),
                actor.clone()
            ),
            UNKNOWN
        );
    }
    assert_eq!(operation.lock().unwrap().phase, Phase::Queued);
    assert_eq!(
        admitted_binding(&run(&handle.ctx, &facts, Request::prepare(prepare), actor)),
        binding
    );
}

#[test]
fn cancellation_waits_for_reserved_but_unpublished_preparation() {
    let world = world(8125);
    let (root, attempt, request) = publish(&world, "d7");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    let slot = PrepareSlot::reserve(
        &world.handle.ctx,
        &request.id,
        world.facts.clock.load(Ordering::SeqCst),
        false,
        AccountSlot::Plain,
    )
    .unwrap();
    world.seal.cancel(recovery::CancellationKind::OwnerStop);
    assert!(!renewal::fixture_settle_terminal_native(
        &world.handle.ctx,
        &world.seal,
        &world.facts
    ));
    assert!(
        world
            .handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .renewal_seals
            .contains_key(&request.id)
    );
    drop(slot);
    assert!(renewal::fixture_settle_terminal_native(
        &world.handle.ctx,
        &world.seal,
        &world.facts
    ));
}

#[test]
fn a_staged_candidate_id_does_not_authorize_predecessor_publication() {
    let world = world(8030);
    let (root, attempt, request) = publish(&world, "d0");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    // If an unauthorized request reached the receipt writer, it would observe
    // this failure as Unknown instead of being rejected before I/O.
    world.facts.receipt_allowed.store(false, Ordering::SeqCst);
    let unauthorized = SupervisorInputCaller {
        principal: Principal::Instance,
        ..world.actor.clone()
    };
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request.clone()),
            unauthorized
        ),
        DENIED
    );
    assert!(
        !world
            .facts
            .journal
            .lock()
            .unwrap()
            .contains_key(&request.id)
    );
    assert!(
        lookup(&world.handle.ctx, &world.old)
            .unwrap()
            .lock()
            .unwrap()
            .closed_restart
            .as_ref()
            .unwrap()
            .available()
    );
}

#[test]
fn cancelled_or_program_replaced_intent_never_gets_fresh_authority() {
    let world = world(7992);
    crate::app_input::note_person_reaching(
        &world.handle.ctx,
        crate::human_input::Gesture::Typed,
        true,
    );
    let (root, attempt, _) = publish(&world, "bc");
    assert!(renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).is_err());
    assert_eq!(world.facts.signals.load(Ordering::Acquire), 1);
    assert_eq!(
        world
            .handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .by_id
            .len(),
        1
    );
}

#[test]
fn wrong_actor_cannot_consume_the_predecessor_and_exact_replay_has_one_child() {
    let world = world(7993);
    let (root, attempt, request) = publish(&world, "bc");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request.clone()),
            caller()
        ),
        DENIED
    );
    let binding = admitted_binding(&run(
        &world.handle.ctx,
        &world.facts,
        Request::prepare(request.clone()),
        world.actor.clone(),
    ));
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request),
            world.actor.clone()
        ),
        admitted(&binding)
    );
    assert_eq!(
        world
            .handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .by_id
            .len(),
        2
    );
    assert!(
        renewal::closed_predecessor(&world.handle.ctx, &world.original.id, &world.facts).is_none(),
        "a transferred predecessor cannot mint another child"
    );
}

#[test]
fn a_different_shell_or_unmatched_terminal_operation_cannot_resume() {
    let world = world(7994);
    let (root, attempt, request) = publish(&world, "bc");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    world
        .facts
        .processes
        .lock()
        .unwrap()
        .get_mut(&400)
        .unwrap()
        .birth += 1;
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request),
            world.actor.clone()
        ),
        DENIED
    );
    assert_eq!(
        world
            .handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .by_id
            .len(),
        1
    );
    let removed = world
        .handle
        .ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .by_id
        .remove(&world.original.id);
    drop(removed);
    assert!(renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).is_err());
}

#[test]
fn explicit_logical_cancellation_cannot_be_cleared_by_later_quiet() {
    let world = world(7995);
    world.seal.cancel(recovery::CancellationKind::OwnerStop);
    let (root, attempt, _) = publish(&world, "bc");
    assert!(renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).is_err());
    assert_eq!(world.facts.signals.load(Ordering::Acquire), 1);
    assert_eq!(
        world
            .handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .by_id
            .len(),
        1
    );
}

#[test]
fn a_changed_original_goal_record_cannot_authorize_the_next_attempt() {
    let world = world(7996);
    let path = world
        ._root
        .path()
        .join("codex-home/alab/sessions/4242.json");
    let mut record: aterm_json::Value =
        aterm_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    record
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
        .insert("goalId".into(), aterm_json::json!("different-goal"));
    std::fs::write(path, record.to_string()).unwrap();
    let (root, attempt, _) = publish(&world, "bc");
    assert!(renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).is_err());
    assert_eq!(
        world
            .handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .by_id
            .len(),
        1
    );
}

#[test]
fn clearing_the_claim_does_not_turn_a_stale_continuation_into_builtin_authority() {
    let world = world(7997);
    let (root, attempt, request) = publish(&world, "bc");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    {
        let mut meta = world.handle.ctx.meta.lock().unwrap();
        meta.supervisor = None;
        assert_eq!(
            meta.supervisor_authority(crate::metrics::now_us())
                .revision(),
            request.revision,
            "the negative control preserves the revision"
        );
    }
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request),
            world.actor.clone()
        ),
        DENIED
    );
    assert!(
        renewal::closed_predecessor(&world.handle.ctx, &world.original.id, &world.facts).is_some()
    );
}

#[test]
fn confirmed_stop_durably_cancels_pending_recovery_when_no_vendor_pid_remains() {
    let world = world(7998);
    world
        .handle
        .ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .renewal_services
        .insert(
            world.root.id.clone(),
            Arc::new(renewal::Service::new(world.seal.clone())),
        );
    assert_eq!(
        crate::app_overview::stop_worker_refusal_in(&world.handle.ctx, false),
        None
    );
    assert_eq!(
        crate::app_overview::stop_worker_refusal_in(&world.handle.ctx, true),
        Some(crate::app_overview::StopRefused::NotRunning)
    );
    assert_eq!(
        crate::app_overview::stop_worker_in(-1, &world.handle.ctx, aterm_slog::Actor::Human),
        Ok("recovery cancelled".into())
    );
    let root = recovery::read_receipt(&world.env, &world.root.id).unwrap();
    assert!(root.revoked);
    assert_eq!(root.phase, recovery::Phase::Retired);
    assert_eq!(
        recovery::read_cancellation(&world.env, &root)
            .unwrap()
            .unwrap()
            .reason,
        recovery::CancellationKind::OwnerStop
    );
    assert_eq!(
        world.facts.signals.load(Ordering::Acquire),
        1,
        "confirmed Stop never attempts to signal the shell"
    );
    assert!(!renewal::pending(&world.handle.ctx));
    assert!(
        renewal::closed_predecessor(&world.handle.ctx, &world.original.id, &world.facts).is_none()
    );
    assert_eq!(
        crate::app_overview::stop_worker_in(-1, &world.handle.ctx, aterm_slog::Actor::Human),
        Err(crate::app_overview::StopRefused::NotRunning)
    );
}

#[test]
fn stop_cancellation_write_failure_keeps_memory_fenced_and_can_be_retried() {
    let world = world(7999);
    world
        .handle
        .ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .renewal_services
        .insert(
            world.root.id.clone(),
            Arc::new(renewal::Service::new(world.seal.clone())),
        );
    let obstruction = recovery::receipt_path(&world.env, &world.root.id)
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("{}.revoked.json", world.root.id));
    std::fs::create_dir(&obstruction).unwrap();
    assert!(matches!(
        crate::app_overview::stop_worker_in(-1, &world.handle.ctx, aterm_slog::Actor::Human),
        Err(crate::app_overview::StopRefused::Signal(_))
    ));
    assert!(
        renewal::pending(&world.handle.ctx),
        "unfinished durable cancellation remains owned"
    );
    assert!(
        renewal::closed_predecessor(&world.handle.ctx, &world.original.id, &world.facts).is_none(),
        "an unavailable ledger cannot restore in-memory execution authority"
    );
    std::fs::remove_dir(&obstruction).unwrap();
    assert_eq!(
        recovery::read_receipt(&world.env, &world.root.id)
            .unwrap()
            .phase,
        recovery::Phase::Committing
    );
    assert_eq!(
        crate::app_overview::stop_worker_in(-1, &world.handle.ctx, aterm_slog::Actor::Human),
        Ok("recovery cancelled".into())
    );
    assert!(
        recovery::read_receipt(&world.env, &world.root.id)
            .unwrap()
            .revoked
    );
    assert!(!renewal::pending(&world.handle.ctx));
    assert_eq!(world.facts.signals.load(Ordering::Acquire), 1);
}

#[test]
fn service_appends_only_after_closed_budget_and_cas_replay_does_not_mint_another_child() {
    let world = world(8000);
    let service = renewal::Service::new(world.seal.clone());
    world
        .facts
        .clock
        .store(world.original.deadline_us - 1, Ordering::Release);
    assert_eq!(
        renewal::fixture_append_child(
            &world.handle.ctx,
            &service,
            &world.root,
            &world.env,
            &world.original,
            &world.facts
        ),
        Err("continuation-budget-live")
    );
    world
        .facts
        .clock
        .store(world.original.deadline_us + 1, Ordering::Release);
    renewal::fixture_append_child(
        &world.handle.ctx,
        &service,
        &world.root,
        &world.env,
        &world.original,
        &world.facts,
    )
    .unwrap();
    let root = recovery::read_receipt(&world.env, &world.root.id).unwrap();
    let attempt = recovery::read_active_attempt(&world.env, &root)
        .unwrap()
        .unwrap();
    let child = recovery::active_prepare(&world.env, &root).unwrap();
    assert_ne!(child.id, world.original.id);
    assert_eq!(
        child.deadline_us,
        world.original.deadline_us + 1 + MAX_PREPARE_US
    );
    assert_eq!(root.native_prepare, world.root.native_prepare);
    assert!(
        renewal::fixture_append_child(
            &world.handle.ctx,
            &service,
            &world.root,
            &world.env,
            &world.original,
            &world.facts
        )
        .is_err()
    );
    assert_eq!(
        recovery::read_receipt(&world.env, &world.root.id).unwrap(),
        root
    );
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    let binding = admitted_binding(&run(
        &world.handle.ctx,
        &world.facts,
        Request::prepare(child),
        world.actor.clone(),
    ));
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::Signal(binding),
            world.actor.clone()
        ),
        DENIED
    );
    assert_eq!(world.facts.signals.load(Ordering::Acquire), 1);
}

#[test]
fn service_skips_only_an_expired_never_admitted_journal_child() {
    let world = world(8001);
    let service = renewal::Service::new(world.seal.clone());
    renewal::fixture_append_child(
        &world.handle.ctx,
        &service,
        &world.root,
        &world.env,
        &world.original,
        &world.facts,
    )
    .unwrap();
    let first = recovery::read_receipt(&world.env, &world.root.id).unwrap();
    let previous = recovery::active_prepare(&world.env, &first).unwrap();
    assert!(
        !world
            .handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .by_id
            .contains_key(&previous.id)
    );
    world
        .facts
        .clock
        .store(previous.deadline_us + 1, Ordering::Release);
    renewal::fixture_append_child(
        &world.handle.ctx,
        &service,
        &first,
        &world.env,
        &previous,
        &world.facts,
    )
    .unwrap();
    let root = recovery::read_receipt(&world.env, &world.root.id).unwrap();
    let attempt = recovery::read_active_attempt(&world.env, &root)
        .unwrap()
        .unwrap();
    assert_eq!(attempt.reference.generation, 2);
    assert_eq!(Some(&attempt.parent), first.current_attempt.as_ref());
    assert_eq!(attempt.native_predecessor, world.original.id);
    let child = recovery::active_prepare(&world.env, &root).unwrap();
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    admitted_binding(&run(
        &world.handle.ctx,
        &world.facts,
        Request::prepare(child),
        world.actor.clone(),
    ));
    assert_eq!(world.facts.watch_count.load(Ordering::Acquire), 1);
    assert_eq!(world.facts.signals.load(Ordering::Acquire), 1);
}

#[test]
fn cancelled_service_releases_staged_seals_and_history_watch_pins() {
    let world = world(8003);
    let proof = lookup(&world.handle.ctx, &world.old)
        .unwrap()
        .lock()
        .unwrap()
        .closed_restart
        .clone()
        .unwrap();
    let (root, attempt, _) = publish(&world, "bc");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    world
        .handle
        .ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .renewal_services
        .insert(
            world.root.id.clone(),
            Arc::new(renewal::Service::new(world.seal.clone())),
        );
    assert_eq!(
        Arc::strong_count(&proof),
        3,
        "native history, staged seal, this observation"
    );
    assert_eq!(
        crate::app_overview::stop_worker_in(-1, &world.handle.ctx, aterm_slog::Actor::Human),
        Ok("recovery cancelled".into())
    );
    // Stop uses the production journal, while this fixture's native operations
    // were issued by Facts. Complete the same cleanup with their owning kernel.
    assert!(renewal::fixture_observe_terminal(
        &world.handle.ctx,
        &world.root.id,
        &world.facts
    ));
    assert!(
        world
            .handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .renewal_seals
            .is_empty()
    );
    assert_eq!(
        Arc::strong_count(&proof),
        2,
        "staged ownership was released"
    );
    let retired = world
        .handle
        .ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .reclaim(world.facts.clock.load(Ordering::Acquire));
    assert_eq!(
        retired.len(),
        1,
        "cancelled completed history is no longer pinned"
    );
    drop(retired);
    assert_eq!(
        Arc::strong_count(&proof),
        1,
        "no hidden service ownership survives history reclamation"
    );
}

#[test]
fn owner_stop_retires_active_signalled_no_go_operation_before_its_deadline() {
    let world = world_with_retirement(8005, false);
    let operation = lookup(&world.handle.ctx, &world.old).unwrap();
    {
        let op = operation.lock().unwrap();
        assert_eq!(op.phase, Phase::Started);
        assert_eq!(op.signal_accepted, Some(true));
        assert!(!op.resume_sent);
        assert!(op.custody.is_some());
        assert!(op.clock.remaining(op.request.deadline_us).is_some());
    }
    world
        .handle
        .ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .renewal_services
        .insert(
            world.root.id.clone(),
            Arc::new(renewal::Service::new(world.seal.clone())),
        );
    crate::app_input::note_person_reaching(
        &world.handle.ctx,
        crate::human_input::Gesture::Other,
        false,
    );
    assert_eq!(
        crate::app_overview::stop_worker_in(-1, &world.handle.ctx, aterm_slog::Actor::Human),
        Ok("recovery cancelled".into())
    );
    reconcile_extinct_by(&world.handle.ctx, &world.facts);
    {
        let op = operation.lock().unwrap();
        assert_eq!(
            op.phase,
            Phase::Completed(Decision::Retired(Retirement::NotGranted))
        );
        assert!(op.custody.is_none());
        assert!(op.completion_drain.as_ref().unwrap().is_drained());
        assert!(
            op.clock.remaining(op.request.deadline_us).is_some(),
            "retirement did not wait for budget expiry"
        );
    }
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::Resume(
                world.old.clone(),
                ImagePhase::Forward,
                "1.1".into(),
                world.handle.ctx.human_input.seq()
            ),
            world.actor.clone()
        ),
        DENIED
    );
    let current = recovery::read_receipt(&world.env, &world.root.id).unwrap();
    assert!(current.revoked);
    assert_eq!(current.phase, recovery::Phase::Retired);
    assert_eq!(
        recovery::read_cancellation(&world.env, &current)
            .unwrap()
            .unwrap()
            .reason,
        recovery::CancellationKind::OwnerStop
    );
    assert!(
        renewal::closed_predecessor(&world.handle.ctx, &world.original.id, &world.facts).is_none()
    );
    assert_eq!(world.facts.signals.load(Ordering::Acquire), 1);
}

#[test]
fn failed_durable_stop_fences_but_does_not_assert_active_operation_terminal() {
    let world = world_with_retirement(8006, false);
    let operation = lookup(&world.handle.ctx, &world.old).unwrap();
    world
        .handle
        .ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .renewal_services
        .insert(
            world.root.id.clone(),
            Arc::new(renewal::Service::new(world.seal.clone())),
        );
    let obstruction = recovery::receipt_path(&world.env, &world.root.id)
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("{}.revoked.json", world.root.id));
    std::fs::create_dir(&obstruction).unwrap();
    assert!(matches!(
        crate::app_overview::stop_worker_in(-1, &world.handle.ctx, aterm_slog::Actor::Human),
        Err(crate::app_overview::StopRefused::Signal(_))
    ));
    reconcile_extinct_by(&world.handle.ctx, &world.facts);
    assert_eq!(operation.lock().unwrap().phase, Phase::Started);
    std::fs::remove_dir(obstruction).unwrap();
    assert_eq!(
        crate::app_overview::stop_worker_in(-1, &world.handle.ctx, aterm_slog::Actor::Human),
        Ok("recovery cancelled".into())
    );
    reconcile_extinct_by(&world.handle.ctx, &world.facts);
    assert_eq!(
        operation.lock().unwrap().phase,
        Phase::Completed(Decision::Retired(Retirement::NotGranted))
    );
    assert_eq!(world.facts.signals.load(Ordering::Acquire), 1);
}

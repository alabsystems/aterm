// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::native_act::{ColdBilling, ColdEnvironmentFacts, ColdIntent};
#[path = "native_memory_relaunch_tests.rs"]
mod memory;

#[derive(Debug)]
struct ResolutionContext(Arc<AtomicBool>);

impl ColdProof for ResolutionContext {
    fn current(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

struct DeferredFacts {
    base: Facts,
    resolution_context: Arc<AtomicBool>,
    policy_current: AtomicBool,
    resolution_calls: AtomicU32,
    revoke_during_admission: AtomicBool,
    admissions: AtomicU32,
    command: RestartCommand,
    actual_launcher: Option<Image>,
    system_clock: bool,
    signal_foreground_gate: Mutex<Option<PublicationGate>>,
}

impl DeferredFacts {
    fn new() -> Self {
        let mut command = restart_command(9);
        command.env.push((
            aterm_agent::harness::accounts::LOGIN_ENV.into(),
            "/selected-account".into(),
        ));
        command.account_intent = Some(Box::new(
            aterm_agent::harness::accounts_pool::ExecutionIntent::Stored {
                namespace: "11".repeat(32),
                login: Some("/selected-account".into()),
                owner: Some("selected-account".into()),
                generation: "22".repeat(32),
            },
        ));
        Self {
            base: Facts::new(),
            resolution_context: Arc::new(AtomicBool::new(true)),
            policy_current: AtomicBool::new(true),
            resolution_calls: AtomicU32::new(0),
            revoke_during_admission: AtomicBool::new(false),
            admissions: AtomicU32::new(0),
            command,
            actual_launcher: None,
            system_clock: false,
            signal_foreground_gate: Mutex::new(None),
        }
    }

    fn serve(
        &self,
        ctx: &Arc<crate::SessionCtx>,
        request: Request,
        actor: SupervisorInputCaller,
    ) -> String {
        serve_by(ctx, -1, request, actor, self, &mut |_| {
            panic!("only the explicit Resume fixture may submit terminal input")
        })
    }
}

impl Kernel for DeferredFacts {
    fn record_successor(
        &self,
        predecessor: &Prepare,
        predecessor_binding: &Binding,
        prepare: &Prepare,
        binding: &Binding,
    ) -> bool {
        self.base
            .record_successor(predecessor, predecessor_binding, prepare, binding)
    }
    fn record_prepared(&self, prepare: &Prepare, binding: &Binding) -> bool {
        self.base.record_prepared(prepare, binding)
    }
    fn record_may_execute(&self, prepare: &Prepare, binding: &Binding) -> bool {
        self.base.record_may_execute(prepare, binding)
    }
    fn record_sealed(&self, prepare: &Prepare, binding: &Binding) -> bool {
        self.base.record_sealed(prepare, binding)
    }
    fn recorded_operation(&self, id: &str) -> Result<bool, ()> {
        self.base.recorded_operation(id)
    }
    fn completion_sealed(&self) {
        self.base.completion_sealed();
    }
    fn record_noexec(&self, prepare: &Prepare, binding: &Binding, phase: ImagePhase) -> bool {
        self.base.record_noexec(prepare, binding, phase)
    }
    fn historical_noexec(&self, request: &Request) -> bool {
        self.base.historical_noexec(request)
    }
    fn historical_closure(&self, prepare: &Prepare) -> Result<Closure, ()> {
        self.base.historical_closure(prepare)
    }
    fn custody(&self) -> Option<String> {
        self.base.custody()
    }
    fn boot(&self) -> Option<BootStamp> {
        self.base.boot()
    }
    fn account_execution(
        &self,
        command: &RestartCommand,
        pid: u32,
        cold: Option<&ColdEnvironmentFacts>,
    ) -> Result<Option<aterm_agent::harness::accounts_pool::ExecutionGuard>, ()> {
        let admitted = self.base.account_execution(command, pid, cold)?;
        if self.revoke_during_admission.load(Ordering::SeqCst)
            && self.admissions.fetch_add(1, Ordering::SeqCst) == 1
        {
            self.resolution_context.store(false, Ordering::SeqCst);
        }
        Ok(admitted)
    }
    fn constructor_enabled(&self, kind: Constructor) -> bool {
        self.base.constructor_enabled(kind)
    }
    fn clock(&self) -> Clock {
        if self.system_clock {
            Clock::System
        } else {
            self.base.clock()
        }
    }
    fn preparing(&self) {
        self.base.preparing();
    }
    fn process(&self, pid: u32) -> Option<Process> {
        self.base.process(pid)
    }
    fn image(&self, image: &Image) -> bool {
        self.base.image(image)
    }
    fn foreground(&self, master: i32) -> Option<u32> {
        let pause = self.signal_foreground_gate.lock().unwrap().take();
        if let Some((entered, gate)) = pause {
            entered.send(()).unwrap();
            let (lock, changed) = &*gate;
            let (released, timeout) = changed
                .wait_timeout_while(lock.lock().unwrap(), PATIENCE, |go| !*go)
                .unwrap();
            assert!(
                *released && !timeout.timed_out(),
                "final foreground probe was not released"
            );
        }
        self.base.foreground(master)
    }
    fn own_target(&self, holder: &str, target: &Image) -> bool {
        self.base.own_target(holder, target)
    }
    fn own_launcher(&self, image: &Image) -> bool {
        self.base.own_launcher(image) || self.actual_launcher.as_ref() == Some(image)
    }
    fn birth_alive(&self, process: &Process) -> Option<bool> {
        self.base.birth_alive(process)
    }
    fn signal(&self, master: i32, process: &Process, ctx: &crate::SessionCtx) -> String {
        self.base.signal(master, process, ctx)
    }
    fn watch(&self, process: &Process) -> Option<DeathWatch> {
        self.base.watch(process)
    }
    fn group_gone(&self, group: u32) -> Option<bool> {
        self.base.group_gone(group)
    }
    fn conversation(&self, ctx: &crate::SessionCtx, vendor: &Process) -> Option<Conversation> {
        self.base.conversation(ctx, vendor)
    }
    fn record_key(&self, ctx: &crate::SessionCtx) -> Option<String> {
        self.base.record_key(ctx)
    }
    fn record_guard(&self, ctx: &crate::SessionCtx) -> Option<Conversation> {
        self.base.record_guard(ctx)
    }
    fn cold_controls_current(&self, _: &ColdIntent) -> bool {
        self.policy_current.load(Ordering::SeqCst)
    }
    fn resolve_cold(
        &self,
        _: &crate::SessionCtx,
        _: &ColdIntent,
        _: &RestartCommand,
        facts: &ColdEnvironmentFacts,
    ) -> Result<(RestartCommand, Arc<dyn ColdProof>), String> {
        assert!(facts.default_home && facts.route.is_stored());
        self.resolution_calls.fetch_add(1, Ordering::SeqCst);
        Ok((
            self.command.clone(),
            Arc::new(ResolutionContext(Arc::clone(&self.resolution_context))),
        ))
    }
}

#[test]
fn deferred_resolution_keeps_the_prepare_and_requires_its_selected_identity_at_execute() {
    use crate::control_input_ack::{InputMark, testing};
    use crate::input::{Delivery, EgressReceipt};

    for scenario in 0..8 {
        let case = if scenario >= 6 { 0 } else { scenario };
        let model = aterm_models_harness::deferred_restart_resolution_model();
        let mut state = model.init_state();
        let (handle, mut reader) = testing::owned_handle(7980 + case);
        let ctx = &handle.ctx;
        if case != 0 {
            install(ctx, 71, &[Scope::Upgrade]);
        }
        // Other cases keep Upgrade with an external holder. The succession case
        // retains both built-in duties so it can attempt the actual next admission.
        assert!(
            ctx.meta
                .lock()
                .unwrap()
                .supervisor_authority(crate::metrics::now_us())
                .builtin_keeps(&crate::harness_host::holder(), Scope::Relaunch)
        );
        let mut kernel = DeferredFacts::new();
        let source = process(4242, 400, 4242, 400, 2);
        let shell = process(400, 1, 400, 400, 4);
        kernel
            .base
            .processes
            .lock()
            .unwrap()
            .insert(source.pid, source.clone());
        kernel
            .base
            .processes
            .lock()
            .unwrap()
            .insert(shell.pid, shell.clone());
        let intent = ColdIntent {
            login: None,
            home: None,
            auth_observed: false,
            route: aterm_agent::harness::accounts::AuthRoute::Stored,
            conversation: Some("captured-conversation".into()),
            profile: None,
            mode: aterm_agent::harness::accounts::Mode::Wall,
            billing: if case == 4 {
                ColdBilling::UsageCredits
            } else {
                ColdBilling::Keep
            },
        };
        let mut prepared = plan(ctx, &kernel.base);
        prepared.holder = crate::harness_host::holder();
        prepared.expected_human_seq = Some(ctx.human_input.seq());
        prepared.duty = Scope::Relaunch;
        prepared.plan = NativePlan::Restart {
            process: source,
            observation: None,
            shell,
            launcher: image(20),
            socket: "/fake-native.sock".into(),
            cold: Some(Box::new(intent.clone())),
            forward: Box::new(restart_command(9)),
            rollback: None,
        };
        let NativePlan::Restart { forward, .. } = &prepared.plan else {
            unreachable!()
        };
        assert_ne!(**forward, kernel.command);
        assert!(forward.cold_resolution_agrees(&kernel.command));
        let original = prepared.clone();
        let builtin = SupervisorInputCaller {
            principal: Principal::Harness,
            connection: Some(8),
            peer: Some(900),
            proof: None,
        };
        let binding =
            admitted_binding(&kernel.serve(ctx, Request::prepare(prepared), builtin.clone()));
        assert!(model.fire("Prepare", &mut state));
        assert_eq!(
            kernel.serve(ctx, Request::Signal(binding.clone()), builtin.clone()),
            "OK native-act state=signalled\n"
        );
        kernel.base.processes.lock().unwrap().remove(&4242);
        kernel.base.foreground.store(400, Ordering::SeqCst);
        let mut pending_input = None;
        let resumed = serve_by(
            ctx,
            -1,
            Request::Resume(binding.clone(), ImagePhase::Forward, "1.1".into(), 0),
            builtin,
            &kernel,
            &mut |_| {
                let mark = InputMark::at_epoch(
                    ctx,
                    handle.local_id,
                    aterm_slog::Actor::Ctl,
                    crate::session_log::InputVerb::Turn,
                    None,
                    true,
                );
                let producer = mark.native_producer();
                if case == 0 {
                    pending_input = Some(mark.clone().pending_supervisor());
                }
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
        assert_eq!(resumed, "OK native-act state=resumed phase=forward\n");
        assert_eq!(testing::byte(&mut reader), b'R');
        let helper = SupervisorInputCaller {
            principal: Principal::Instance,
            connection: Some(80),
            peer: Some(600),
            proof: None,
        };
        kernel
            .base
            .processes
            .lock()
            .unwrap()
            .insert(600, process(600, 400, 600, 400, 20));
        kernel.base.foreground.store(600, Ordering::SeqCst);
        let attached = kernel.serve(
            ctx,
            Request::Attach(binding.clone(), ImagePhase::Forward),
            helper.clone(),
        );
        assert!(matches!(Reply::parse(&attached),
            Some(Reply::NeedEnvironment { deadline_us, .. }) if deadline_us == original.deadline_us));
        let encoded_intent = attached
            .split_ascii_whitespace()
            .find_map(|word| word.strip_prefix("intent="))
            .unwrap();
        let intent_json =
            String::from_utf8(aterm_codec::hex::decode_vec(encoded_intent.as_bytes()).unwrap())
                .unwrap();
        let old_reader: aterm_json::Value = aterm_json::from_str(&intent_json).unwrap();
        assert_eq!(
            old_reader
                .get("stored_login")
                .and_then(aterm_json::Value::as_bool),
            Some(true)
        );
        assert!(
            old_reader.get("route").is_none(),
            "the captured older launcher cannot read route"
        );
        assert_eq!(ColdIntent::parse(&intent_json), Some(intent.clone()));
        assert_eq!(
            kernel.serve(
                ctx,
                Request::Execute(binding.clone(), ImagePhase::Forward),
                helper.clone()
            ),
            DENIED
        );
        assert!(model.fire("ExecuteUnresolved", &mut state));
        assert!(model.fire("ChildReady", &mut state));

        // This is a verified helper's observation of its own empty environment, not an
        // unreadable kernel snapshot that happened to supply no entries.
        let facts =
            ColdEnvironmentFacts::observe(&[], &intent, std::path::Path::new("/fixture")).unwrap();
        assert!(model.fire("ObserveEmptyHelper", &mut state));
        let resolve = Request::Resolve(binding.clone(), ImagePhase::Forward, facts);
        if case == 3 {
            kernel.resolution_context.store(false, Ordering::SeqCst);
            assert_eq!(kernel.serve(ctx, resolve.clone(), helper.clone()), DENIED);
            assert_eq!(kernel.resolution_calls.load(Ordering::SeqCst), 1);
            kernel.resolution_context.store(true, Ordering::SeqCst);
            assert_eq!(
                kernel.serve(ctx, resolve, helper.clone()),
                DENIED,
                "a refused Resolve cannot select again for the same launched helper"
            );
            assert_eq!(kernel.resolution_calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                kernel.serve(
                    ctx,
                    Request::Execute(binding.clone(), ImagePhase::Forward),
                    helper
                ),
                DENIED
            );
            let operation = lookup(ctx, &binding).unwrap();
            assert_eq!(operation.lock().unwrap().request, original);
            assert!(
                !operation.lock().unwrap().children[0]
                    .as_ref()
                    .unwrap()
                    .execute_sent
            );
            continue;
        }
        let resolved = kernel.serve(ctx, resolve.clone(), helper.clone());
        assert!(
            matches!(Reply::parse(&resolved), Some(Reply::ResolvedLaunch { command, deadline_us })
            if command == kernel.command && deadline_us == original.deadline_us)
        );
        assert!(model.fire("Resolve", &mut state));
        assert_eq!(kernel.serve(ctx, resolve, helper.clone()), resolved);
        if case == 0 {
            assert!(model.fire("DuplicateResolve", &mut state));
        }
        assert_eq!(kernel.resolution_calls.load(Ordering::SeqCst), 1);
        let operation = lookup(ctx, &binding).unwrap();
        assert_eq!(operation.lock().unwrap().request, original);
        match case {
            1 => {
                assert!(kernel.resolution_context.load(Ordering::SeqCst));
                kernel.base.account_allowed.store(false, Ordering::SeqCst);
                assert!(model.fire("RebindSamePath", &mut state));
            }
            2 => {
                kernel
                    .base
                    .clock
                    .store(original.deadline_us, Ordering::SeqCst);
                assert!(model.fire("Deadline", &mut state));
            }
            4 => {
                assert!(kernel.base.account_allowed.load(Ordering::SeqCst));
                kernel.policy_current.store(false, Ordering::SeqCst);
                assert!(model.fire("Revoke", &mut state));
            }
            5 => {
                // The issuer first reserves a grant, journals it, then admits again to
                // deliver it. Invalidate during that final writer wait, after the second
                // metadata observation, so the journal's extra check cannot mask it.
                assert!(kernel.resolution_context.load(Ordering::SeqCst));
                kernel.revoke_during_admission.store(true, Ordering::SeqCst);
            }
            _ => (),
        }
        let execute = kernel.serve(
            ctx,
            Request::Execute(binding.clone(), ImagePhase::Forward),
            helper,
        );
        if case == 0 {
            assert!(model.fire("Execute", &mut state));
            assert!(
                matches!(Reply::parse(&execute), Some(Reply::Execute { deadline_us })
                if deadline_us == original.deadline_us)
            );
        } else {
            if case != 5 {
                assert!(model.fire(
                    match case {
                        1 => "ExecuteReboundIdentity",
                        4 => "ExecuteRevoked",
                        _ => "ExecuteExpired",
                    },
                    &mut state
                ));
            } else {
                assert_eq!(kernel.admissions.load(Ordering::SeqCst), 2);
                assert!(!kernel.resolution_context.load(Ordering::SeqCst));
            }
            assert_eq!(execute, DENIED);
            assert!(
                !operation.lock().unwrap().children[0]
                    .as_ref()
                    .unwrap()
                    .execute_sent
            );
        }
        assert_eq!(operation.lock().unwrap().request, original);
        assert_eq!(kernel.resolution_calls.load(Ordering::SeqCst), 1);
        if case == 0 {
            // Complete the actual issued image, then exercise the same terminal append
            // used by the RPC server. A completed phase alone must not mint succession.
            kernel
                .base
                .processes
                .lock()
                .unwrap()
                .get_mut(&600)
                .unwrap()
                .image = kernel.command.image.clone();
            let completion_actor = SupervisorInputCaller {
                principal: Principal::Harness,
                connection: Some(8),
                peer: Some(900),
                proof: None,
            };
            assert_eq!(
                kernel.serve(
                    ctx,
                    Request::Precommit(binding.clone(), ImagePhase::Forward),
                    completion_actor.clone()
                ),
                "OK native-act state=precommitted\n"
            );
            let mut asked = original.clone();
            asked.id = "cd".repeat(16);
            asked.duty = Scope::Upgrade;
            asked.revision = ctx
                .meta
                .lock()
                .unwrap()
                .supervisor_authority(crate::metrics::now_us())
                .revision();
            asked.asked = Some(aterm_agent::harness::native_act::AskedIntent::Upgrade(
                aterm_agent::harness::native_act::AskedUpgradeIntent {
                    predecessor: binding.id.clone(),
                },
            ));
            let mut asked_command = kernel.command.clone();
            asked_command.image = image(10);
            asked_command.program = "/image-10".into();
            asked.plan = NativePlan::Restart {
                process: kernel
                    .base
                    .processes
                    .lock()
                    .unwrap()
                    .get(&600)
                    .unwrap()
                    .clone(),
                shell: process(400, 1, 400, 400, 4),
                observation: None,
                cold: None,
                launcher: image(20),
                socket: "/fake-native.sock".into(),
                forward: Box::new(asked_command.clone()),
                rollback: Some(Box::new(kernel.command.clone())),
            };
            // The old host snapshot is unavailable while the cold image is being
            // adopted. Actual kernel completion must not fabricate passive provenance.
            assert!(operation.lock().unwrap().committed_image.is_none());
            kernel
                .base
                .conversation_valid
                .store(false, Ordering::SeqCst);
            assert_eq!(
                kernel.serve(
                    ctx,
                    Request::Commit(binding.clone(), ImagePhase::Forward, "2.1.318".into()),
                    completion_actor.clone()
                ),
                UNKNOWN
            );
            assert!(operation.lock().unwrap().committed_image.is_some());
            assert!(operation.lock().unwrap().completed_observation.is_none());
            assert!(operation.lock().unwrap().terminal_acknowledged.is_none());
            assert!(
                !operation
                    .lock()
                    .unwrap()
                    .completion_drain
                    .as_ref()
                    .unwrap()
                    .is_drained()
            );
            assert_eq!(
                kernel.serve(
                    ctx,
                    Request::prepare(asked.clone()),
                    completion_actor.clone()
                ),
                DENIED
            );
            // Restoring a real current record permits fresh checks, but does not
            // settle held custody or acknowledge the terminal journal by itself.
            kernel.base.conversation_valid.store(true, Ordering::SeqCst);
            {
                let mut table = ctx.fenced_writes.native.lock().unwrap();
                let _settlement = table.settle_asked_binding();
                assert!(matches!(
                    table.asked_binding,
                    asked_upgrade::CurrentBinding::Unsettled { .. }
                ));
            }
            // Only the real pending input's release can drain its custody. A subsequent
            // failed durable terminal publication still cannot establish the positive slot.
            drop(pending_input.take());
            assert!(
                operation
                    .lock()
                    .unwrap()
                    .completion_drain
                    .as_ref()
                    .unwrap()
                    .is_drained()
            );
            kernel
                .base
                .seal_journal_allowed
                .store(false, Ordering::SeqCst);
            assert_eq!(
                kernel.serve(
                    ctx,
                    Request::Query(binding.clone()),
                    completion_actor.clone()
                ),
                UNKNOWN
            );
            assert!(operation.lock().unwrap().terminal_acknowledged.is_none());
            assert_eq!(
                kernel.serve(
                    ctx,
                    Request::prepare(asked.clone()),
                    completion_actor.clone()
                ),
                DENIED
            );
            kernel
                .base
                .seal_journal_allowed
                .store(true, Ordering::SeqCst);
            let reply = kernel.serve(
                ctx,
                Request::Query(binding.clone()),
                completion_actor.clone(),
            );
            assert!(
                matches!(
                    Reply::parse(&reply),
                    Some(Reply::ColdRestartCommitted { .. })
                ),
                "{reply}"
            );
            assert_eq!(
                operation.lock().unwrap().terminal_acknowledged,
                Some(Decision::Committed)
            );
            {
                let mut table = ctx.fenced_writes.native.lock().unwrap();
                let _settlement = table.settle_asked_binding();
                assert!(matches!(
                    table.asked_binding,
                    asked_upgrade::CurrentBinding::Settled(_)
                ));
            }
            // Even fully settled history cannot replace current conversation facts.
            // A different never-published asked candidate may expire without consuming
            // this genuine settlement. The live request below must still qualify afresh.
            let mut expired = asked.clone();
            expired.id = "ef".repeat(16);
            expired.deadline_us = kernel.base.clock.load(Ordering::SeqCst);
            let expected = aterm_agent::harness::native_act::unadmitted_reply(&expired.id).unwrap();
            assert_eq!(
                kernel.serve(
                    ctx,
                    Request::prepare(expired.clone()),
                    completion_actor.clone()
                ),
                expected
            );
            {
                let table = ctx.fenced_writes.native.lock().unwrap();
                assert!(!table.by_id.contains_key(&expired.id));
                assert!(!table.reservations.contains_key(&expired.id));
                assert!(matches!(
                    table.asked_binding,
                    asked_upgrade::CurrentBinding::Settled(_)
                ));
            }
            assert_eq!(kernel.base.signals.load(Ordering::SeqCst), 1);
            kernel
                .base
                .conversation_valid
                .store(false, Ordering::SeqCst);
            assert_eq!(
                kernel.serve(
                    ctx,
                    Request::prepare(asked.clone()),
                    completion_actor.clone()
                ),
                DENIED
            );
            kernel.base.conversation_valid.store(true, Ordering::SeqCst);
            kernel.base.account_allowed.store(false, Ordering::SeqCst);
            assert_eq!(
                kernel.serve(
                    ctx,
                    Request::prepare(asked.clone()),
                    completion_actor.clone()
                ),
                DENIED
            );
            kernel.base.account_allowed.store(true, Ordering::SeqCst);
            let aborted = abort_retry_before_effect(ctx, &mut kernel, &asked, &completion_actor);
            let next = admitted_binding(&kernel.serve(
                ctx,
                Request::prepare(asked.clone()),
                completion_actor.clone(),
            ));
            assert_eq!(lookup(ctx, &next).unwrap().lock().unwrap().request, asked);
            assert_eq!(
                kernel.base.signals.load(Ordering::SeqCst),
                1,
                "new preparation neither signals nor launches another image"
            );
            // A late terminal replay from the old operation cannot revive its positive
            // fact after an actually accepted successor has replaced the current slot.
            let _old_reply = kernel.serve(
                ctx,
                Request::Query(binding.clone()),
                completion_actor.clone(),
            );
            assert_eq!(
                kernel.serve(ctx, Request::Query(aborted), completion_actor.clone()),
                ROLLBACK_SAFE
            );
            let mut table = ctx.fenced_writes.native.lock().unwrap();
            let _settlement = table.settle_asked_binding();
            assert!(
                matches!(&table.asked_binding, asked_upgrade::CurrentBinding::Unsettled { binding, .. } if *binding == next)
            );
            assert!(table.account_lineages.pins(&next.id));
            drop(table);

            // Drive the genuinely admitted Stored Asked root through its own helper
            // and known kernel completion. Hold only the existing post-seal gate.
            assert_eq!(
                kernel.serve(ctx, Request::Signal(next.clone()), completion_actor.clone()),
                "OK native-act state=signalled\n"
            );
            assert_eq!(
                kernel.serve(ctx, Request::Abort(next.clone()), completion_actor.clone()),
                UNKNOWN
            );
            {
                let mut table = ctx.fenced_writes.native.lock().unwrap();
                assert!(table.settle_asked_binding().is_err());
                assert!(matches!(&table.asked_binding,
                    asked_upgrade::CurrentBinding::Unsettled { binding, .. } if *binding == next));
            }
            kernel.base.processes.lock().unwrap().remove(&600);
            kernel.base.foreground.store(400, Ordering::SeqCst);
            if scenario == 6 {
                assert_eq!(
                    kernel.serve(
                        ctx,
                        Request::Reconcile(next.clone()),
                        completion_actor.clone()
                    ),
                    NOEXEC
                );
                let mut table = ctx.fenced_writes.native.lock().unwrap();
                assert!(table.settle_asked_binding().is_err());
                assert!(matches!(&table.asked_binding,
                    asked_upgrade::CurrentBinding::Unsettled { binding, .. } if *binding == next));
                continue;
            }
            let resumed = serve_by(
                ctx,
                -1,
                Request::Resume(next.clone(), ImagePhase::Forward, "1.1".into(), 0),
                completion_actor.clone(),
                &kernel,
                &mut |_| {
                    let mark = InputMark::at_epoch(
                        ctx,
                        handle.local_id,
                        aterm_slog::Actor::Ctl,
                        crate::session_log::InputVerb::Turn,
                        None,
                        true,
                    );
                    let producer = mark.native_producer();
                    let completion = mark.completion();
                    assert!(completion.start_supervised());
                    let receipt = ctx.sink.write_frame_with_prefix(b"S").unwrap();
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
            assert_eq!(resumed, "OK native-act state=resumed phase=forward\n");
            assert_eq!(testing::byte(&mut reader), b'S');
            kernel
                .base
                .processes
                .lock()
                .unwrap()
                .insert(700, process(700, 400, 700, 400, 20));
            kernel.base.foreground.store(700, Ordering::SeqCst);
            let helper = SupervisorInputCaller {
                principal: Principal::Instance,
                connection: Some(81),
                peer: Some(700),
                proof: None,
            };
            assert!(matches!(
                Reply::parse(&kernel.serve(
                    ctx,
                    Request::Attach(next.clone(), ImagePhase::Forward),
                    helper.clone()
                )),
                Some(Reply::Launch { .. })
            ));
            if scenario == 7 {
                kernel
                    .base
                    .grant_journal_allowed
                    .store(false, Ordering::SeqCst);
                assert_eq!(
                    kernel.serve(
                        ctx,
                        Request::Execute(next.clone(), ImagePhase::Forward),
                        helper
                    ),
                    UNKNOWN
                );
                kernel.base.processes.lock().unwrap().remove(&700);
                kernel.base.foreground.store(400, Ordering::SeqCst);
                assert_eq!(
                    kernel.serve(
                        ctx,
                        Request::Reconcile(next.clone()),
                        completion_actor.clone()
                    ),
                    UNKNOWN
                );
                let op = lookup(ctx, &next).unwrap();
                assert_eq!(
                    op.lock().unwrap().phase,
                    Phase::Completed(Decision::Unknown)
                );
                let mut table = ctx.fenced_writes.native.lock().unwrap();
                assert!(table.settle_asked_binding().is_err());
                assert!(matches!(&table.asked_binding,
                    asked_upgrade::CurrentBinding::Unsettled { binding, .. } if *binding == next));
                continue;
            }
            assert!(matches!(
                Reply::parse(&kernel.serve(
                    ctx,
                    Request::Execute(next.clone(), ImagePhase::Forward),
                    helper
                )),
                Some(Reply::Execute { .. })
            ));
            kernel
                .base
                .processes
                .lock()
                .unwrap()
                .get_mut(&700)
                .unwrap()
                .image = asked_command.image.clone();
            assert_eq!(
                kernel.serve(
                    ctx,
                    Request::Precommit(next.clone(), ImagePhase::Forward),
                    completion_actor.clone()
                ),
                "OK native-act state=precommitted\n"
            );
            let (arrived, entered) = mpsc::sync_channel(1);
            let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
            kernel.base.outcome_gate = Some((arrived, Arc::clone(&gate)));
            let kernel = Arc::new(kernel);
            let worker_kernel = Arc::clone(&kernel);
            let worker_ctx = Arc::clone(ctx);
            let worker_actor = completion_actor.clone();
            let worker_binding = next.clone();
            let (finished, returned) = mpsc::sync_channel(1);
            let worker = std::thread::spawn(move || {
                let reply = worker_kernel.serve(
                    &worker_ctx,
                    Request::Commit(worker_binding, ImagePhase::Forward, "2.1.319".into()),
                    worker_actor,
                );
                let _sent = finished.send(reply);
            });
            let reached = entered.recv_timeout(PATIENCE);
            let held = reached.as_ref().ok().and_then(|_| {
                let table = ctx
                    .fenced_writes
                    .native
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let operation = table.by_id.get(&next.id)?;
                let operation = operation.lock().unwrap_or_else(|p| p.into_inner());
                Some((
                    operation.phase,
                    operation.terminal_acknowledged,
                    operation
                        .completion_drain
                        .as_ref()
                        .map(|drain| drain.is_drained()),
                    table.account_lineages.pins(&next.id),
                ))
            });
            if reached.is_ok() {
                kernel.base.clock.store(asked.deadline_us, Ordering::SeqCst);
            }
            {
                let (lock, changed) = &*gate;
                *lock.lock().unwrap_or_else(|p| p.into_inner()) = true;
                changed.notify_all();
            }
            let reply = returned
                .recv_timeout(PATIENCE)
                .expect("late terminal worker did not return");
            worker.join().unwrap();
            assert!(reached.is_ok(), "the actual post-seal gate was not reached");
            assert_eq!(
                held,
                Some((
                    Phase::Completed(Decision::Committed),
                    None,
                    Some(false),
                    true
                ))
            );
            assert!(
                matches!(
                    Reply::parse(&reply),
                    Some(Reply::RestartCommitted(_)) | Some(Reply::Committed(_))
                ),
                "{reply}"
            );
            let (retired, removed, settled) = {
                let mut table = ctx.fenced_writes.native.lock().unwrap();
                let retired = table.reclaim(kernel.base.clock.load(Ordering::SeqCst));
                (
                    retired,
                    !table.by_id.contains_key(&next.id),
                    matches!(
                        &table.asked_binding,
                        asked_upgrade::CurrentBinding::Settled(_)
                    ),
                )
            };
            drop(retired);
            assert!(removed, "the expired operation was actually reclaimed");
            assert!(
                settled,
                "compacting the Stored root must preserve its settled successor fact"
            );
            let mut fresh = asked;
            fresh.id = "de".repeat(16);
            fresh.deadline_us = kernel.base.clock.load(Ordering::SeqCst) + 60_000_000;
            let Some(AskedIntent::Upgrade(intent)) = fresh.asked.as_mut() else {
                panic!("the successor is an asked upgrade");
            };
            intent.predecessor = next.id;
            let NativePlan::Restart {
                process,
                forward,
                rollback,
                ..
            } = &mut fresh.plan
            else {
                unreachable!();
            };
            *process = kernel.base.process(700).unwrap();
            *rollback = Some(Box::new(asked_command));
            forward.image = image(11);
            forward.program = "/image-11".into();
            let admitted = admitted_binding(&kernel.serve(
                ctx,
                Request::prepare(fresh),
                completion_actor.clone(),
            ));
            assert_eq!(
                kernel.serve(ctx, Request::Abort(admitted), completion_actor),
                ROLLBACK_SAFE
            );
        }
    }
}

// Reuse the genuine completed cold predecessor and actual next admission. No terminal
// phase, acknowledgement or settled data is supplied by this fixture.
fn abort_retry_before_effect(
    ctx: &Arc<crate::SessionCtx>,
    kernel: &mut DeferredFacts,
    asked: &Prepare,
    actor: &SupervisorInputCaller,
) -> Binding {
    let mut abandoned = asked.clone();
    abandoned.id = "ac".repeat(16);
    let binding = admitted_binding(&kernel.serve(ctx, Request::prepare(abandoned), actor.clone()));
    let (arrived, entered) = mpsc::sync_channel(1);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    kernel.base.outcome_gate = Some((arrived, Arc::clone(&gate)));
    kernel
        .base
        .seal_journal_allowed
        .store(false, Ordering::SeqCst);
    let (reached, reply, joined, denied_held) = std::thread::scope(|scope| {
        let shared = &*kernel;
        let worker_binding = binding.clone();
        let worker_actor = actor.clone();
        let (finished, returned) = mpsc::sync_channel(1);
        let worker = scope.spawn(move || {
            let reply = shared.serve(ctx, Request::Abort(worker_binding), worker_actor);
            let _sent = finished.send(reply);
        });
        let reached = entered.recv_timeout(PATIENCE);
        let denied_held = reached.as_ref().ok().map(|_| {
            let undrained = lookup(ctx, &binding).is_some_and(|op| {
                op.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .completion_drain
                    .as_ref()
                    .is_some_and(|drain| !drain.is_drained())
            });
            let reply = shared.serve(ctx, Request::prepare(asked.clone()), actor.clone());
            (undrained, reply)
        });
        {
            let (lock, changed) = &*gate;
            *lock.lock().unwrap_or_else(|p| p.into_inner()) = true;
            changed.notify_all();
        }
        let reply = returned.recv_timeout(PATIENCE);
        let joined = worker.join();
        (reached, reply, joined, denied_held)
    });
    kernel.base.outcome_gate = None;
    kernel.base.completion_seen.store(false, Ordering::SeqCst);
    assert!(joined.is_ok(), "actual Abort worker panicked");
    assert!(
        reached.is_ok(),
        "actual Abort post-seal gate was not reached"
    );
    assert_eq!(denied_held, Some((true, DENIED.into())));
    assert_eq!(reply.unwrap(), UNKNOWN);
    assert_eq!(
        kernel.serve(ctx, Request::prepare(asked.clone()), actor.clone()),
        DENIED
    );
    let op = lookup(ctx, &binding).unwrap();
    assert_eq!(op.lock().unwrap().terminal_acknowledged, None);
    kernel
        .base
        .seal_journal_allowed
        .store(true, Ordering::SeqCst);
    assert_eq!(
        kernel.serve(ctx, Request::Query(binding.clone()), actor.clone()),
        ROLLBACK_SAFE
    );
    assert_eq!(
        op.lock().unwrap().terminal_acknowledged,
        Some(Decision::ProvenNotCommittedRollbackSafe)
    );
    let NativePlan::Restart { process, .. } = &asked.plan else {
        unreachable!();
    };
    let original = kernel
        .base
        .processes
        .lock()
        .unwrap()
        .get(&process.pid)
        .unwrap()
        .clone();
    kernel
        .base
        .processes
        .lock()
        .unwrap()
        .get_mut(&process.pid)
        .unwrap()
        .birth += 1;
    let changed = kernel.serve(ctx, Request::prepare(asked.clone()), actor.clone());
    kernel
        .base
        .processes
        .lock()
        .unwrap()
        .insert(process.pid, original);
    assert_eq!(
        changed, DENIED,
        "restored history cannot authorize a replacement source"
    );
    // The caller next performs the actual fresh admission against the still-live original.
    binding
}

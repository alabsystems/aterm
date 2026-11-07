// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::native_act::{ColdBilling, ColdEnvironmentFacts, ColdIntent};

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
    fn record_noexec(&self, prepare: &Prepare, binding: &Binding, phase: ImagePhase) -> bool {
        self.base.record_noexec(prepare, binding, phase)
    }
    fn historical_noexec(&self, request: &Request) -> bool {
        self.base.historical_noexec(request)
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
        self.base.clock()
    }
    fn process(&self, pid: u32) -> Option<Process> {
        self.base.process(pid)
    }
    fn image(&self, image: &Image) -> bool {
        self.base.image(image)
    }
    fn foreground(&self, master: i32) -> Option<u32> {
        self.base.foreground(master)
    }
    fn own_target(&self, holder: &str, target: &Image) -> bool {
        self.base.own_target(holder, target)
    }
    fn own_launcher(&self, image: &Image) -> bool {
        self.base.own_launcher(image)
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

    for case in 0..6 {
        let model = aterm_models_harness::deferred_restart_resolution_model();
        let mut state = model.init_state();
        let (handle, mut reader) = testing::owned_handle(7980 + case);
        let ctx = &handle.ctx;
        install(ctx, 71, &[Scope::Upgrade]);
        // The external holder owns Upgrade only; this private constructor must retain
        // Relaunch with the built-in harness rather than taking another holder's duty.
        assert!(
            ctx.meta
                .lock()
                .unwrap()
                .supervisor_authority(crate::metrics::now_us())
                .builtin_keeps(&crate::harness_host::holder(), Scope::Relaunch)
        );
        let kernel = DeferredFacts::new();
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
    }
}

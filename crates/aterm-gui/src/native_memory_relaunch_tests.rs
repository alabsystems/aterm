// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::relaunch::{
    CheckedRelaunch, CheckedRelaunchStep, Restart, RestartAsk, checked_fixture::World,
};

#[path = "native_memory_signal_fence_tests.rs"]
mod signal_fence;

#[path = "native_memory_unadmitted_tests.rs"]
mod unadmitted;

// Kernel PIDs are positive signed integers. Keep the synthetic controller outside
// that range so inserting a real resumed child cannot replace its authority.
const CONTROLLER_PID: u32 = u32::MAX;

fn builtin() -> SupervisorInputCaller {
    SupervisorInputCaller {
        principal: Principal::Harness,
        connection: Some(8),
        peer: Some(CONTROLLER_PID),
        proof: None,
    }
}

/// Actual cold controller, helper grant, kernel outcome, terminal append and custody drain.
/// Kernel observations are injected; this is not a physical cold-exec claim.
fn complete_cold_source(
    handle: &crate::session_store::SessionHandle,
    reader: &mut std::fs::File,
    world: &World,
) -> (DeferredFacts, Prepare, Binding, Reply) {
    use crate::control_input_ack::{InputMark, testing};
    use crate::input::{Delivery, EgressReceipt};
    let ctx = &handle.ctx;
    let mut kernel = DeferredFacts::new();
    {
        let mut processes = kernel.base.processes.lock().unwrap();
        processes.remove(&900);
        processes.insert(
            CONTROLLER_PID,
            process(CONTROLLER_PID, 1, CONTROLLER_PID, 400, 20),
        );
    }
    // The real producer captures this image independently when it builds its recipe.
    // Keep the synthetic helper image too; neither path admits an arbitrary launcher.
    kernel.actual_launcher = Some(Image::at(&std::env::current_exe().unwrap()).unwrap());
    // Both ends of the real producer exchange use the same monotonic clock. A frozen
    // earlier Facts time would make its fresh full-budget deadline exceed the wire bound.
    kernel.system_clock = true;
    let account_intent = kernel.command.account_intent.clone();
    kernel.base.clock.store(
        aterm_agent::harness::native_act::monotonic_us().unwrap(),
        Ordering::SeqCst,
    );
    kernel.command = RestartCommand {
        image: world.process().image.clone(),
        program: world.snapshot().program.to_string_lossy().into_owned(),
        argv: vec!["--resume".into(), world.session().into()],
        env: Vec::new(),
        unset: None,
        cwd: None,
        heal: None,
        dialect: aterm_agent::harness::upgrade::Dialect::Bash,
        account_intent,
        settles: None,
    };
    let mut old_pid = world.process().pid.checked_add(1_000_000).unwrap();
    while [world.process().pid, world.shell().pid, CONTROLLER_PID].contains(&old_pid) {
        old_pid = old_pid.checked_add(1).unwrap();
    }
    let mut old = process(
        old_pid,
        world.shell().pid,
        old_pid,
        world.shell().session,
        2,
    );
    old.birth = world
        .process()
        .birth
        .checked_sub(1)
        .filter(|birth| *birth != 0)
        .unwrap();
    {
        let mut processes = kernel.base.processes.lock().unwrap();
        processes.insert(old.pid, old.clone());
        processes.insert(world.shell().pid, world.shell().clone());
    }
    kernel.base.foreground.store(old.pid, Ordering::SeqCst);
    let intent = ColdIntent {
        login: None,
        home: None,
        auth_observed: false,
        route: aterm_agent::harness::accounts::AuthRoute::Stored,
        conversation: Some(world.session().into()),
        profile: None,
        mode: aterm_agent::harness::accounts::Mode::Wall,
        billing: ColdBilling::Keep,
    };
    let mut prepare = plan(ctx, &kernel.base);
    prepare.holder = crate::harness_host::holder();
    prepare.expected_human_seq = Some(ctx.human_input.seq());
    prepare.duty = Scope::Relaunch;
    prepare.deadline_us = kernel
        .base
        .clock
        .load(Ordering::SeqCst)
        .checked_add(120_000_000)
        .unwrap();
    prepare.plan = NativePlan::Restart {
        process: old.clone(),
        observation: None,
        shell: world.shell().clone(),
        launcher: image(20),
        socket: "/private-fixture.sock".into(),
        forward: Box::new(kernel.command.clone()),
        rollback: None,
        cold: Some(Box::new(intent.clone())),
    };
    let binding =
        admitted_binding(&kernel.serve(ctx, Request::prepare(prepare.clone()), builtin()));
    assert_eq!(
        kernel.serve(ctx, Request::Signal(binding.clone()), builtin()),
        "OK native-act state=signalled\n"
    );
    kernel.base.processes.lock().unwrap().remove(&old.pid);
    kernel
        .base
        .foreground
        .store(world.shell().group, Ordering::SeqCst);
    let resumed = serve_by(
        ctx,
        -1,
        Request::Resume(binding.clone(), ImagePhase::Forward, "1.1".into(), 0),
        builtin(),
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
    assert_eq!(testing::byte(reader), b'R');
    let mut helper = world.process().clone();
    helper.image = image(20);
    kernel
        .base
        .processes
        .lock()
        .unwrap()
        .insert(helper.pid, helper.clone());
    kernel.base.foreground.store(helper.group, Ordering::SeqCst);
    let caller = SupervisorInputCaller {
        principal: Principal::Instance,
        connection: Some(80),
        peer: Some(helper.pid),
        proof: None,
    };
    assert!(matches!(
        Reply::parse(&kernel.serve(
            ctx,
            Request::Attach(binding.clone(), ImagePhase::Forward),
            caller.clone()
        )),
        Some(Reply::NeedEnvironment { .. })
    ));
    let environment =
        ColdEnvironmentFacts::observe(&[], &intent, std::path::Path::new("/fixture")).unwrap();
    assert!(matches!(
        Reply::parse(&kernel.serve(
            ctx,
            Request::Resolve(binding.clone(), ImagePhase::Forward, environment),
            caller.clone()
        )),
        Some(Reply::ResolvedLaunch { .. })
    ));
    assert!(matches!(
        Reply::parse(&kernel.serve(
            ctx,
            Request::Execute(binding.clone(), ImagePhase::Forward),
            caller
        )),
        Some(Reply::Execute { .. })
    ));
    kernel
        .base
        .processes
        .lock()
        .unwrap()
        .insert(world.process().pid, world.process().clone());
    assert_eq!(
        kernel.serve(
            ctx,
            Request::Precommit(binding.clone(), ImagePhase::Forward),
            builtin()
        ),
        "OK native-act state=precommitted\n"
    );
    let reply = Reply::parse(&kernel.serve(
        ctx,
        Request::Commit(binding.clone(), ImagePhase::Forward, "1.0.0".into()),
        builtin(),
    ))
    .unwrap();
    assert!(matches!(reply, Reply::AdmittedRestartCommitted { .. }));
    world.retain_completion(&prepare, &binding, &reply).unwrap();
    kernel.base.signals.store(0, Ordering::SeqCst);
    (kernel, prepare, binding, reply)
}

#[test]
fn checked_memory_uses_real_source_gates_and_never_reauthorizes_recorded_signal() {
    use crate::control_input_ack::testing;
    for case in 0..5 {
        let (handle, mut reader) = testing::owned_handle(7988 + case);
        let ctx = Arc::clone(&handle.ctx);
        let mut world = World::new(
            aterm_types::tab_id::TabId::parse(ctx.self_id.as_str())
                .expect("the owned handle has a minted tab ID"),
        )
        .unwrap();
        crate::harness_host::publish_snapshot_for_test(
            ctx.self_id.as_str(),
            Some(world.snapshot()),
        );
        let (kernel, cold, _, _) = complete_cold_source(&handle, &mut reader, &world);
        let kernel = Arc::new(kernel);
        let controls = world.controls();
        controls.revision(
            ctx.meta
                .lock()
                .unwrap()
                .supervisor_authority(crate::metrics::now_us())
                .revision(),
        );
        // Memory alone may replace its own in-process work. Background/draft still hold it.
        controls.screen(
            &[
                "✻ Working…",
                "────────────────────",
                "❯ ",
                "────────────────────",
            ],
            2,
            2,
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let prepare_replies = Arc::new(Mutex::new(Vec::new()));
        let memory_binding = Arc::new(Mutex::new(None::<Binding>));
        let cross_session_refused = Arc::new(AtomicBool::new(false));
        let wrong = crate::session_store::test_handle_on(8090 + case, -1);
        let authorizer = {
            let ctx = Arc::clone(&ctx);
            let calls = Arc::clone(&calls);
            let cross_session_refused = Arc::clone(&cross_session_refused);
            move |checked: &CheckedRelaunch| {
                cross_session_refused.store(
                    crate::native_act::authorize_checked_relaunch(&wrong.ctx, checked).is_err(),
                    Ordering::SeqCst,
                );
                calls
                    .lock()
                    .unwrap()
                    .push((checked.step(), checked.request().clone()));
                if case == 0 && checked.step() == CheckedRelaunchStep::Prepare {
                    // P1 is genuinely issued and revoked; its delayed exact cleanup
                    // must not erase P2 even though both name the same recipe.
                    return crate::native_act::relaunch_permit::authorize_after_old_cleanup(
                        &ctx, checked,
                    );
                }
                crate::native_act::authorize_checked_relaunch(&ctx, checked)
            }
        };
        let native = {
            let ctx = Arc::clone(&ctx);
            let kernel = Arc::clone(&kernel);
            let controls = controls.clone();
            let prepare_replies = Arc::clone(&prepare_replies);
            let memory_binding = Arc::clone(&memory_binding);
            move |line: &str| {
                let Some((_, wire)) = line.split_once(" native-act ") else {
                    return "ERR fixture native routing\n".into();
                };
                let Some(request) = Request::parse(wire) else {
                    return "ERR fixture native grammar\n".into();
                };
                let new_prepare = matches!(&request, Request::Prepare(prepare) if matches!(prepare.asked, Some(AskedIntent::Relaunch(_))));
                let reply = kernel.serve(&ctx, request, builtin());
                if new_prepare {
                    prepare_replies.lock().unwrap().push(if reply == DENIED {
                        "denied"
                    } else if reply == UNKNOWN {
                        "unknown"
                    } else if matches!(Reply::parse(&reply), Some(Reply::Admitted(_))) {
                        "admitted"
                    } else if matches!(Reply::parse(&reply), Some(Reply::Pending(_))) {
                        "pending"
                    } else if reply == "ERR busy unresolved native operation\n" {
                        "busy-unresolved"
                    } else if reply == "ERR busy native operation prepare\n" {
                        "busy-prepare"
                    } else if reply == "ERR busy native operation history\n" {
                        "busy-history"
                    } else if reply == "ERR busy native passive outcome capacity\n" {
                        "busy-passive"
                    } else if reply == "ERR busy app handoff\n" {
                        "busy-handoff"
                    } else if reply == "ERR native operation deadline\n" {
                        "deadline"
                    } else if reply == "ERR native constructor disabled\n" {
                        "disabled"
                    } else {
                        "other"
                    });
                }
                if new_prepare && let Some(Reply::Admitted(binding)) = Reply::parse(&reply) {
                    *memory_binding.lock().unwrap() = Some(binding);
                    match case {
                        1 => controls.screen(
                            &[
                                "────────────────────",
                                "❯ keep this draft",
                                "────────────────────",
                            ],
                            1,
                            17,
                        ),
                        2 => controls.background(&["owned shell job"]),
                        3 => {
                            ctx.human_input
                                .note(crate::metrics::now_us(), crate::human_input::Gesture::Typed);
                            controls.human_sequence(ctx.human_input.seq());
                        }
                        4 => kernel.base.account_allowed.store(false, Ordering::SeqCst),
                        _ => {}
                    }
                }
                reply
            }
        };
        let ask = RestartAsk::of(&Restart::Memory, 17);
        let answer = world.run(&Restart::Memory, &ask, &authorizer, native);
        let callbacks_before_read = calls.lock().unwrap().len();
        let read_without_new_authority = if case == 0 {
            ctx.human_input
                .note(crate::metrics::now_us(), crate::human_input::Gesture::Typed);
            controls.human_sequence(ctx.human_input.seq());
            controls.background(&["new work after recorded Signal"]);
            controls.screen(
                &[
                    "────────────────────",
                    "❯ new draft",
                    "────────────────────",
                ],
                1,
                12,
            );
            let ctx = Arc::clone(&ctx);
            let read_kernel = Arc::clone(&kernel);
            let requests_before_read = controls.requests().len();
            let _read = world.run(&Restart::Memory, &ask, &authorizer, move |line| {
                line.split_once(" native-act ")
                    .and_then(|(_, wire)| Request::parse(wire))
                    .map_or_else(
                        || "ERR fixture native grammar\n".into(),
                        |request| read_kernel.serve(&ctx, request, builtin()),
                    )
            });
            calls.lock().unwrap().len() == callbacks_before_read
                && kernel.base.signals.load(Ordering::SeqCst) == 1
                && memory_binding.lock().unwrap().as_ref().is_some_and(|expected| {
                    controls.requests().iter().skip(requests_before_read)
                        .filter_map(|line| line.split_once(" native-act "))
                        .filter_map(|(_, wire)| Request::parse(wire))
                        .any(|request| matches!(request, Request::Query(binding) if binding == *expected))
                })
        } else {
            true
        };
        let observed = calls.lock().unwrap().clone();
        let signals = kernel.base.signals.load(Ordering::SeqCst);
        let source_alive = world.process().pid;
        let source_present = kernel
            .base
            .processes
            .lock()
            .unwrap()
            .contains_key(&source_alive);
        let exact = observed.iter().all(|(_, request)| request.duty == Scope::Relaunch
            && matches!(&request.asked, Some(AskedIntent::Relaunch(intent)) if intent.predecessor == cold.id && intent.ask == ask.word())
            && matches!(&request.plan, NativePlan::Restart {process, ..} if process == world.process()));
        let retry_after_veto = if case != 0 {
            controls.background(&[]);
            controls.screen(
                &["────────────────────", "❯ ", "────────────────────"],
                1,
                2,
            );
            controls.human_sequence(ctx.human_input.seq());
            kernel.base.account_allowed.store(true, Ordering::SeqCst);
            let retry_ctx = Arc::clone(&ctx);
            let retry_kernel = Arc::clone(&kernel);
            let _retry = world.run(&Restart::Memory, &ask, &authorizer, move |line| {
                line.split_once(" native-act ")
                    .and_then(|(_, wire)| Request::parse(wire))
                    .map_or_else(
                        || "ERR fixture native grammar\n".into(),
                        |request| retry_kernel.serve(&retry_ctx, request, builtin()),
                    )
            });
            let all = calls.lock().unwrap();
            kernel.base.signals.load(Ordering::SeqCst) == 1
                && all
                    .iter()
                    .filter(|(step, _)| *step == CheckedRelaunchStep::Prepare)
                    .count()
                    == 2
                && all
                    .first()
                    .zip(all.last())
                    .is_some_and(|(first, last)| first.1.id != last.1.id)
        } else {
            true
        };
        let closes = world.close();
        kernel.base.processes.lock().unwrap().remove(&source_alive);
        kernel
            .base
            .foreground
            .store(world.shell().group, Ordering::SeqCst);
        let operations = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .by_id
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for operation in operations {
            let binding = operation.lock().unwrap().binding.clone();
            kernel.serve(&ctx, Request::Reconcile(binding), builtin());
        }
        crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), None);
        assert!(closes.is_ok(), "fixture did not retire");
        assert!(
            cross_session_refused.load(Ordering::SeqCst),
            "producer returned {} before checked authorization",
            answer.report.step
        );
        assert!(
            exact && !observed.is_empty(),
            "actual producer must reach the typed Memory preparation"
        );
        assert!(
            source_present,
            "the fake kernel never substitutes an absent source"
        );
        assert_eq!(
            signals,
            u32::from(case == 0),
            "case {case}: {}; Prepare replies {:?}; callback count {}",
            answer.report.step,
            prepare_replies.lock().unwrap(),
            observed.len()
        );
        assert_eq!(
            observed
                .iter()
                .filter(|(step, _)| *step == CheckedRelaunchStep::Signal)
                .count(),
            usize::from(case == 0 || case == 4)
        );
        assert!(!answer.report.step.is_empty());
        assert!(
            read_without_new_authority,
            "recorded Signal is observed despite later unsafe source state"
        );
        assert!(
            retry_after_veto,
            "positive no-effect closure permits a fresh checked retry after the source clears"
        );
    }
}

#[test]
fn uncertain_memory_preparation_rechecks_the_source_and_reuses_the_saved_recipe() {
    use crate::control_input_ack::testing;
    let (handle, mut reader) = testing::owned_handle(8110);
    let ctx = Arc::clone(&handle.ctx);
    let mut world = World::new(
        aterm_types::tab_id::TabId::parse(ctx.self_id.as_str())
            .expect("the owned handle has a minted tab ID"),
    )
    .unwrap();
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), Some(world.snapshot()));
    let (kernel, _, _, _) = complete_cold_source(&handle, &mut reader, &world);
    let kernel = Arc::new(kernel);
    let controls = world.controls();
    controls.revision(
        ctx.meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us())
            .revision(),
    );
    kernel
        .base
        .prepare_journal_ack
        .store(false, Ordering::SeqCst);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let authorizer = {
        let ctx = Arc::clone(&ctx);
        let kernel = Arc::clone(&kernel);
        let requests = Arc::clone(&requests);
        move |checked: &CheckedRelaunch| {
            let scope = crate::native_act::authorize_checked_relaunch(&ctx, checked)?;
            if checked.step() == CheckedRelaunchStep::Prepare {
                let mut seen = requests.lock().unwrap();
                seen.push(checked.request().clone());
                if seen.len() == 2 {
                    kernel
                        .base
                        .prepare_journal_ack
                        .store(true, Ordering::SeqCst);
                }
            }
            Ok(scope)
        }
    };
    let ask = RestartAsk::of(&Restart::Memory, 18);
    let run = |world: &mut World| {
        let ctx = Arc::clone(&ctx);
        let kernel = Arc::clone(&kernel);
        world.run(&Restart::Memory, &ask, &authorizer, move |line| {
            line.split_once(" native-act ")
                .and_then(|(_, wire)| Request::parse(wire))
                .map_or_else(
                    || "ERR fixture native grammar\n".into(),
                    |request| kernel.serve(&ctx, request, builtin()),
                )
        })
    };
    let _unknown = run(&mut world);
    let queued = {
        let requests = requests.lock().unwrap();
        requests
            .first()
            .and_then(|request| {
                ctx.fenced_writes
                    .native
                    .lock()
                    .unwrap()
                    .by_id
                    .get(&request.id)
                    .cloned()
            })
            .is_some_and(|operation| operation.lock().unwrap().phase == Phase::Queued)
    };
    let no_first_signal = kernel.base.signals.load(Ordering::SeqCst) == 0;
    let _retry = run(&mut world);
    let same_recipe = {
        let requests = requests.lock().unwrap();
        requests.len() == 2 && requests[0] == requests[1]
    };
    let signals = kernel.base.signals.load(Ordering::SeqCst);
    let closed = world.close();
    kernel
        .base
        .processes
        .lock()
        .unwrap()
        .remove(&world.process().pid);
    kernel
        .base
        .foreground
        .store(world.shell().group, Ordering::SeqCst);
    let bindings = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .by_id
        .values()
        .map(|operation| operation.lock().unwrap().binding.clone())
        .collect::<Vec<_>>();
    for binding in bindings {
        kernel.serve(&ctx, Request::Reconcile(binding), builtin());
    }
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), None);
    assert!(closed.is_ok());
    assert!(
        queued && no_first_signal,
        "uncertain append retains its unsignalled operation"
    );
    assert!(
        same_recipe,
        "fresh producer checks cannot renew an uncertain recipe"
    );
    assert_eq!(signals, 1);
}

#[test]
fn revoked_memory_producer_during_journal_closes_without_effect_and_allows_fresh_retry() {
    use crate::control_input_ack::testing;
    let (handle, mut reader) = testing::owned_handle(8111);
    let ctx = Arc::clone(&handle.ctx);
    let mut world = World::new(
        aterm_types::tab_id::TabId::parse(ctx.self_id.as_str())
            .expect("the owned handle has a minted tab ID"),
    )
    .unwrap();
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), Some(world.snapshot()));
    let (mut kernel, _, _, _) = complete_cold_source(&handle, &mut reader, &world);
    world.controls().revision(
        ctx.meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us())
            .revision(),
    );
    let (entered, arrival) = std::sync::mpsc::sync_channel(1);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    kernel.base.prepare_journal_gate = Some((entered, Arc::clone(&gate)));
    let kernel = Arc::new(kernel);
    let producer = Arc::new(Mutex::new(None::<Box<dyn Send>>));
    let first_request = Arc::new(Mutex::new(None::<Prepare>));
    let (finished, completion) = std::sync::mpsc::channel();
    let worker = {
        let ctx = Arc::clone(&ctx);
        let kernel = Arc::clone(&kernel);
        let producer = Arc::clone(&producer);
        let first_request = Arc::clone(&first_request);
        std::thread::spawn(move || {
            let authorizer = |checked: &CheckedRelaunch| {
                let scope = crate::native_act::authorize_checked_relaunch(&ctx, checked)?;
                *first_request.lock().unwrap() = Some(checked.request().clone());
                *producer.lock().unwrap() = Some(scope);
                Ok(Box::new(()) as Box<dyn Send>)
            };
            let native_ctx = Arc::clone(&ctx);
            let _answer = world.run(
                &Restart::Memory,
                &RestartAsk::of(&Restart::Memory, 19),
                &authorizer,
                move |line| {
                    line.split_once(" native-act ")
                        .and_then(|(_, wire)| Request::parse(wire))
                        .map_or_else(
                            || "ERR fixture native grammar\n".into(),
                            |request| kernel.serve(&native_ctx, request, builtin()),
                        )
                },
            );
            let _ = finished.send(());
            world
        })
    };
    let arrived = arrival.recv_timeout(PATIENCE);
    let transferred = first_request
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|request| {
            ctx.fenced_writes
                .native
                .lock()
                .unwrap()
                .by_id
                .get(&request.id)
                .cloned()
                .is_some_and(|operation| operation.lock().unwrap().phase == Phase::Queued)
        });
    drop(producer.lock().unwrap().take());
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    let completed = completion.recv_timeout(PATIENCE);
    // Gate release and scope revocation happen even on a failed arrival. Never join a
    // worker that failed its completion detector.
    completed.expect("revoked preparation worker did not finish");
    let mut world = worker.join().unwrap();
    let no_signal = kernel.base.signals.load(Ordering::SeqCst) == 0;
    let fresh = Arc::new(Mutex::new(None::<Prepare>));
    let authorizer = {
        let ctx = Arc::clone(&ctx);
        let fresh = Arc::clone(&fresh);
        move |checked: &CheckedRelaunch| {
            if checked.step() == CheckedRelaunchStep::Prepare {
                *fresh.lock().unwrap() = Some(checked.request().clone());
            }
            crate::native_act::authorize_checked_relaunch(&ctx, checked)
        }
    };
    let native_ctx = Arc::clone(&ctx);
    let native_kernel = Arc::clone(&kernel);
    let _retry = world.run(
        &Restart::Memory,
        &RestartAsk::of(&Restart::Memory, 19),
        &authorizer,
        move |line| {
            line.split_once(" native-act ")
                .and_then(|(_, wire)| Request::parse(wire))
                .map_or_else(
                    || "ERR fixture native grammar\n".into(),
                    |request| native_kernel.serve(&native_ctx, request, builtin()),
                )
        },
    );
    let distinct_retry = first_request
        .lock()
        .unwrap()
        .as_ref()
        .zip(fresh.lock().unwrap().as_ref())
        .is_some_and(|(old, new)| old.id != new.id);
    let signals = kernel.base.signals.load(Ordering::SeqCst);
    let closed = world.close();
    kernel
        .base
        .processes
        .lock()
        .unwrap()
        .remove(&world.process().pid);
    kernel
        .base
        .foreground
        .store(world.shell().group, Ordering::SeqCst);
    let bindings = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .by_id
        .values()
        .map(|operation| operation.lock().unwrap().binding.clone())
        .collect::<Vec<_>>();
    for binding in bindings {
        kernel.serve(&ctx, Request::Reconcile(binding), builtin());
    }
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), None);
    assert!(closed.is_ok());
    assert!(arrived.is_ok() && transferred && no_signal);
    assert!(
        distinct_retry,
        "known no-effect closure restores historical eligibility, not the old permit"
    );
    assert_eq!(signals, 1);
}

/// A MEMORY RESTART RE-ENTERS A HOST THAT SETTLED NONE OF ITS PREDECESSORS FROM THAT HOST'S OWN
/// REGISTRATION, UNDER ITS CHECKED PRODUCER (a coordinated session's memory restart after a
/// seamless update named the predecessor the previous host process settled, which the new one
/// cannot prove: denied `settlement-unknown`, replayed to its deadline, and minted again at every
/// trigger): once the host's table is fresh, its succession answer is `reentry`, the agent's
/// checked relaunch names the host's own current-epoch registration of the live source with its
/// ask, and the host admits it under the producer's scoped authorization.
///
/// NEGATIVE CONTROL: with the checked relaunch naming the record's predecessor whatever the host
/// answers, the prepare names that predecessor and the host denies it.
#[test]
fn a_memory_restart_reenters_a_recreated_host_from_its_own_registration() {
    memory_reentry(8133, false);
}

/// A MEMORY RESTART OF A RECORD THAT COMPLETED NO NATIVE OPERATION RE-ENTERS (a conversation whose
/// first native operation was cancelled before any effect stays coordinated for good, and its
/// Memory restart waited `native-asked-predecessor-unproven` at the gate and in the plan, before
/// any host read, though nothing could mint that predecessor): its succession read names no
/// predecessor, the host that binds nothing answers `reentry`, and its checked relaunch re-enters
/// from the host's own registration, which the host admits under the checked producer.
///
/// NEGATIVE CONTROL: with the gate requiring a completed predecessor again, the restart waits
/// `native-asked-predecessor-unproven` and prepares nothing.
#[test]
fn a_memory_restart_of_a_record_that_completed_no_native_operation_reenters() {
    memory_reentry(8137, true);
}

/// A Memory restart through the real gate and checked producer of a world whose host process was
/// recreated, its record holding the cold source's completion, or (`uncompleted`) only its no-exec
/// fact: the host's succession read answers `reentry`, and its re-entry is prepared and admitted.
fn memory_reentry(local: u64, uncompleted: bool) {
    use crate::control_input_ack::testing;
    use aterm_agent::harness::native_act::AskedReentryIntent;
    use aterm_agent::harness::native_observe::{Registration, Reply as ObserveReply};
    let (handle, mut reader) = testing::owned_handle(local);
    let ctx = Arc::clone(&handle.ctx);
    let mut world = World::new(
        aterm_types::tab_id::TabId::parse(ctx.self_id.as_str())
            .expect("the owned handle has a minted tab ID"),
    )
    .unwrap();
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), Some(world.snapshot()));
    let (facts, cold, _, _) = complete_cold_source(&handle, &mut reader, &world);
    if uncompleted {
        world.uncompleted().unwrap();
    }
    world.controls().revision(
        ctx.meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us())
            .revision(),
    );
    // The host process is recreated: its fresh table settled nothing the record names.
    *ctx.fenced_writes.native.lock().unwrap() = Operations::default();
    // It registers the live source passively, and the record's observer keeps the binding.
    let registration = Registration {
        id: "7d".repeat(16),
        host_epoch: host_epoch().unwrap().to_string(),
        vendor: world.process().clone(),
        shell: world.shell().clone(),
    };
    let context = {
        let meta = ctx.meta.lock().unwrap();
        observe_control::context(&ctx, &meta, &builtin(), &facts).unwrap()
    };
    let registered = observe_control::store(&ctx).lock().unwrap().register_with(
        &registration,
        &context,
        -1,
        facts.base.clock.load(Ordering::SeqCst),
        &ObservationKernel(&facts),
        || Some("5".repeat(64)),
    );
    let ObserveReply::Registered(binding) = registered else {
        panic!("the live source registers: {registered:?}");
    };
    world.observed(registration, binding.clone()).unwrap();
    let kernel = Arc::new(facts);
    let exchanges = Arc::new(Mutex::new(Vec::new()));
    let native = {
        let ctx = Arc::clone(&ctx);
        let kernel = Arc::clone(&kernel);
        let exchanges = Arc::clone(&exchanges);
        move |line: &str| {
            let Some(request) = line
                .split_once(" native-act ")
                .and_then(|(_, wire)| Request::parse(wire))
            else {
                return "ERR fixture native grammar\n".into();
            };
            if matches!(&request, Request::Prepare(_)) {
                kernel.base.clock.store(
                    aterm_agent::harness::native_act::monotonic_us().unwrap(),
                    Ordering::SeqCst,
                );
            }
            let reply = kernel.serve(&ctx, request.clone(), builtin());
            exchanges.lock().unwrap().push((request, reply.clone()));
            reply
        }
    };
    let authorizer =
        |checked: &CheckedRelaunch| crate::native_act::authorize_checked_relaunch(&ctx, checked);
    let ask = RestartAsk::of(&Restart::Memory, 33);
    let _answer = world.run(&Restart::Memory, &ask, &authorizer, native);
    let exchanges = exchanges.lock().unwrap().clone();
    let closed = world.close();
    kernel
        .base
        .processes
        .lock()
        .unwrap()
        .remove(&world.process().pid);
    kernel
        .base
        .foreground
        .store(world.shell().group, Ordering::SeqCst);
    let bindings = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .by_id
        .values()
        .map(|operation| operation.lock().unwrap().binding.clone())
        .collect::<Vec<_>>();
    for binding in bindings {
        kernel.serve(&ctx, Request::Reconcile(binding), builtin());
    }
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), None);
    assert!(closed.is_ok());
    let named = (!uncompleted).then(|| cold.id.clone());
    assert!(
        exchanges.iter().any(|(request, reply)| {
            *request == Request::Succession(named.clone()) && reply.contains(" admits=reentry")
        }),
        "the recreated host answers re-entry after {named:?}: {exchanges:?}"
    );
    let (prepare, reply) = exchanges
        .iter()
        .find_map(|(request, reply)| match request {
            Request::Prepare(prepare) => Some((prepare, reply)),
            _ => None,
        })
        .expect("the memory restart prepared");
    assert_eq!(
        prepare.asked,
        Some(AskedIntent::Reentry(AskedReentryIntent {
            observation: binding,
            ask: Some(ask.word().to_owned()),
        })),
        "{exchanges:?}"
    );
    assert_eq!(prepare.duty, Scope::Relaunch);
    assert!(
        matches!(Reply::parse(reply), Some(Reply::Admitted(_))),
        "admitted under the checked producer: {reply}"
    );
}

/// A MEMORY RESTART HELD AT THE GATE NOTES WHAT HOLDS IT IN THE RECORD'S WAIT LOG (the gate's
/// reason was noted on the round's own fresh record, which a restart saves only once it signals
/// the agent, so the reason reached aterm.log alone and `aterm harness upgrade --status` showed no
/// wait for it): with the settled vendor out of the foreground, the host's succession read answers
/// `bound` at `qualification-foreground`, the restart waits `native-reentry-bound` and prepares
/// nothing, and the record it loaded keeps that wait with the host's stage.
///
/// NEGATIVE CONTROL: with the gate's wait noted on the round's own record again, the saved wait
/// log holds no `native-reentry-bound`.
#[test]
fn a_memory_restart_held_at_the_gate_notes_the_hosts_stage_in_the_records_wait_log() {
    use crate::control_input_ack::testing;
    let (handle, mut reader) = testing::owned_handle(8138);
    let ctx = Arc::clone(&handle.ctx);
    let mut world = World::new(
        aterm_types::tab_id::TabId::parse(ctx.self_id.as_str())
            .expect("the owned handle has a minted tab ID"),
    )
    .unwrap();
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), Some(world.snapshot()));
    let (facts, _, _, _) = complete_cold_source(&handle, &mut reader, &world);
    world.controls().revision(
        ctx.meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us())
            .revision(),
    );
    // The settled vendor lives, its shell's group in the foreground.
    facts
        .base
        .foreground
        .store(world.shell().group, Ordering::SeqCst);
    let kernel = Arc::new(facts);
    let exchanges = Arc::new(Mutex::new(Vec::new()));
    let native = {
        let ctx = Arc::clone(&ctx);
        let kernel = Arc::clone(&kernel);
        let exchanges = Arc::clone(&exchanges);
        move |line: &str| {
            let Some(request) = line
                .split_once(" native-act ")
                .and_then(|(_, wire)| Request::parse(wire))
            else {
                return "ERR fixture native grammar\n".into();
            };
            let reply = kernel.serve(&ctx, request.clone(), builtin());
            exchanges.lock().unwrap().push((request, reply.clone()));
            reply
        }
    };
    let authorizer =
        |checked: &CheckedRelaunch| crate::native_act::authorize_checked_relaunch(&ctx, checked);
    let ask = RestartAsk::of(&Restart::Memory, 33);
    let answer = world.run(&Restart::Memory, &ask, &authorizer, native);
    let exchanges = exchanges.lock().unwrap().clone();
    let waits = world.waits();
    let closed = world.close();
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), None);
    assert!(closed.is_ok());
    assert_eq!(
        answer.report.step, "wait:native-reentry-bound",
        "{exchanges:?}"
    );
    assert!(
        exchanges.iter().any(|(request, reply)| {
            matches!(request, Request::Succession(Some(_)))
                && reply.contains(" admits=bound why=qualification-foreground")
        }),
        "{exchanges:?}"
    );
    assert!(
        !exchanges
            .iter()
            .any(|(request, _)| matches!(request, Request::Prepare(_))),
        "nothing is prepared: {exchanges:?}"
    );
    let (word, note) = waits.last().expect("the record keeps the gate's wait");
    assert_eq!(word, "native-reentry-bound", "{waits:?}");
    assert!(note.contains("qualification-foreground"), "{waits:?}");
}

/// A SETTLEMENT WHOSE VENDOR A PERSON REPLACED IS RE-ENTERED, NEVER NAMED (the person quit Claude
/// Code in the tab and resumed the same conversation by hand: the host kept the settlement of the
/// vendor that exited, its succession read answered `predecessor` on the settlement's id alone,
/// and the agent told and prepared an upgrade naming it, which qualification denied
/// (`qualification-resulting-vendor`), replayed to its deadline and minted again for the rest of
/// the epoch): the read judges what qualification judges. While the settled vendor lives but is
/// not in the foreground, the answer is `bound`, and the session waits with no tell; once it is
/// positively gone, nothing that runs can inherit the settlement, so the epoch binds nothing, the
/// answer is `reentry`, and the host admits a re-entry from its own registration of the vendor
/// the person started. While the settled vendor lives in the foreground, the answer stays
/// `predecessor`.
///
/// NEGATIVE CONTROLS: with the read answering `predecessor` on the settlement's id, receipt
/// flight and lineage alone, the backgrounded and replaced vendors both answer `predecessor`;
/// with a settlement whose vendor is gone kept bound, the answer is `bound` and the re-entry is
/// DENIED; with a read that names no predecessor judged as one naming the settlement, it answers
/// `predecessor` to a record that never held it.
#[test]
fn a_settlement_whose_vendor_a_person_replaced_is_reentered_never_named() {
    use crate::control_input_ack::testing;
    use aterm_agent::harness::native_act::{AskedReentryIntent, Succession, succession_reply};
    use aterm_agent::harness::native_observe::{Registration, Reply as ObserveReply};
    let (handle, mut reader) = testing::owned_handle(8134);
    let ctx = Arc::clone(&handle.ctx);
    let world = World::new(
        aterm_types::tab_id::TabId::parse(ctx.self_id.as_str())
            .expect("the owned handle has a minted tab ID"),
    )
    .unwrap();
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), Some(world.snapshot()));
    let (kernel, prepare, settled, _) = complete_cold_source(&handle, &mut reader, &world);
    let succession = || {
        kernel.serve(
            &ctx,
            Request::Succession(Some(settled.id.clone())),
            builtin(),
        )
    };
    let answer = |admits: Succession| succession_reply(host_epoch().unwrap(), &admits).unwrap();
    // CONTROL: the settled vendor live in the foreground is named.
    assert_eq!(succession(), answer(Succession::Predecessor));
    // A record that names no predecessor is never answered this settlement: it is another's.
    assert_eq!(
        kernel.serve(&ctx, Request::Succession(None), builtin()),
        answer(Succession::Bound("succession-another-predecessor".into()))
    );
    // The settled vendor lives, its shell's group in the foreground: qualification would refuse
    // the foreground, so nothing is told or prepared.
    kernel
        .base
        .foreground
        .store(world.shell().group, Ordering::SeqCst);
    assert_eq!(
        succession(),
        answer(Succession::Bound("qualification-foreground".into()))
    );
    kernel
        .base
        .foreground
        .store(world.process().group, Ordering::SeqCst);
    assert_eq!(succession(), answer(Succession::Predecessor));

    // The person quits the settled vendor and resumes the conversation by hand.
    let mut replaced = world.process().clone();
    replaced.pid = world.process().pid.checked_add(2_000_000).unwrap();
    while [world.shell().pid, CONTROLLER_PID].contains(&replaced.pid)
        || kernel
            .base
            .processes
            .lock()
            .unwrap()
            .contains_key(&replaced.pid)
    {
        replaced.pid = replaced.pid.checked_add(1).unwrap();
    }
    replaced.group = replaced.pid;
    replaced.birth = world.process().birth.checked_add(1).unwrap();
    {
        let mut processes = kernel.base.processes.lock().unwrap();
        processes.remove(&world.process().pid);
        processes.insert(replaced.pid, replaced.clone());
    }
    kernel
        .base
        .foreground
        .store(replaced.group, Ordering::SeqCst);
    assert_eq!(succession(), answer(Succession::Reentry));

    // The host's own registration of the vendor the person started re-enters the epoch.
    let registration = Registration {
        id: "7c".repeat(16),
        host_epoch: host_epoch().unwrap().to_string(),
        vendor: replaced.clone(),
        shell: world.shell().clone(),
    };
    let context = {
        let meta = ctx.meta.lock().unwrap();
        observe_control::context(&ctx, &meta, &builtin(), &kernel).unwrap()
    };
    let registered = observe_control::store(&ctx).lock().unwrap().register_with(
        &registration,
        &context,
        -1,
        kernel.base.clock.load(Ordering::SeqCst),
        &ObservationKernel(&kernel),
        || Some("5".repeat(64)),
    );
    let ObserveReply::Registered(observation) = registered else {
        panic!("the replaced vendor registers: {registered:?}");
    };
    let mut reentry = prepare;
    reentry.id = "5d".repeat(16);
    reentry.duty = Scope::Upgrade;
    reentry.revision = ctx
        .meta
        .lock()
        .unwrap()
        .supervisor_authority(crate::metrics::now_us())
        .revision();
    reentry.expected_human_seq = Some(ctx.human_input.seq());
    reentry.deadline_us = aterm_agent::harness::native_act::monotonic_us()
        .unwrap()
        .checked_add(60_000_000)
        .unwrap();
    reentry.asked = Some(AskedIntent::Reentry(AskedReentryIntent {
        observation,
        ask: None,
    }));
    let mut forward = kernel.command.clone();
    forward.image = image(10);
    forward.program = "/image-10".into();
    reentry.plan = NativePlan::Restart {
        process: replaced,
        observation: None,
        shell: world.shell().clone(),
        launcher: image(20),
        socket: "/private-fixture.sock".into(),
        forward: Box::new(forward),
        rollback: Some(Box::new(kernel.command.clone())),
        cold: None,
    };
    assert!(reentry.asked_restart_shape());
    let reply = kernel.serve(&ctx, Request::prepare(reentry.clone()), builtin());
    assert_eq!(admitted_binding(&reply).id, reentry.id, "{reply}");
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), None);
}

/// This host's own current-epoch registration of `vendor` under `world`'s shell, made under the
/// metadata guards that stand now, as the passive observer makes it.
fn register_now(
    ctx: &crate::SessionCtx,
    kernel: &DeferredFacts,
    world: &World,
    vendor: &Process,
    id: &str,
    capability: &str,
) -> aterm_agent::harness::native_observe::ObservationBinding {
    use aterm_agent::harness::native_observe::{Registration, Reply as ObserveReply};
    let registration = Registration {
        id: id.into(),
        host_epoch: host_epoch().unwrap().to_string(),
        vendor: vendor.clone(),
        shell: world.shell().clone(),
    };
    let context = {
        let meta = ctx.meta.lock().unwrap();
        observe_control::context(ctx, &meta, &builtin(), kernel).unwrap()
    };
    let registered = observe_control::store(ctx).lock().unwrap().register_with(
        &registration,
        &context,
        -1,
        kernel.base.clock.load(Ordering::SeqCst),
        &ObservationKernel(kernel),
        || Some(capability.into()),
    );
    let ObserveReply::Registered(observation) = registered else {
        panic!("the live vendor registers: {registered:?}");
    };
    observation
}

/// A re-entry `id` of `vendor` under `world`'s shell, from the settled cold source's `prepare`,
/// named by `observation` and stamped with the claim revision and person's sequence that stand.
fn reentry_now(
    ctx: &crate::SessionCtx,
    kernel: &DeferredFacts,
    world: &World,
    prepare: &Prepare,
    vendor: &Process,
    observation: aterm_agent::harness::native_observe::ObservationBinding,
    id: &str,
) -> Prepare {
    let mut reentry = prepare.clone();
    reentry.id = id.into();
    reentry.duty = Scope::Upgrade;
    reentry.revision = ctx
        .meta
        .lock()
        .unwrap()
        .supervisor_authority(crate::metrics::now_us())
        .revision();
    reentry.expected_human_seq = Some(ctx.human_input.seq());
    reentry.deadline_us = aterm_agent::harness::native_act::monotonic_us()
        .unwrap()
        .checked_add(60_000_000)
        .unwrap();
    reentry.asked = Some(AskedIntent::Reentry(
        aterm_agent::harness::native_act::AskedReentryIntent {
            observation,
            ask: None,
        },
    ));
    let mut forward = kernel.command.clone();
    forward.image = image(10);
    forward.program = "/image-10".into();
    reentry.plan = NativePlan::Restart {
        process: vendor.clone(),
        observation: None,
        shell: world.shell().clone(),
        launcher: image(20),
        socket: "/private-fixture.sock".into(),
        forward: Box::new(forward),
        rollback: Some(Box::new(kernel.command.clone())),
        cold: None,
    };
    assert!(reentry.asked_restart_shape());
    reentry
}

/// A SETTLEMENT WHOSE BUILT-IN RELAUNCH CONTINUITY BROKE IS RE-ENTERED FROM A FRESH REGISTRATION
/// (a peer's `aterm ctl` claim taking Relaunch came and went while the settled Claude Code kept
/// running: the claim and its release each advanced the built-in relaunch generation, which only
/// advances, so the settlement could never again be what qualification requires, yet its vendor
/// lived, so the host kept it and answered `bound`, refused every re-entry
/// `qualification-reentry-bound`, and the session's upgrades and Memory restarts waited
/// `native-reentry-bound` until the person quit Claude Code or aterm was updated): a settlement
/// nothing that runs can inherit binds nothing, so once the built-in harness keeps Relaunch again
/// in a later generation the answer is `reentry`, and the host admits a re-entry only from its
/// own registration made under that generation, never from one made before the break. While the
/// foreign claim stands, the settlement holds the epoch (`qualification-foreign-relaunch`), as it
/// does while its vendor reads as nothing yet is not proved gone.
///
/// NEGATIVE CONTROL: with the settlement kept while its vendor lives (`settle_current` releasing
/// only a vendor positively gone), the answer after the claim's release is `bound`
/// (`qualification-builtin-continuity`).
#[test]
fn a_settlement_whose_relaunch_continuity_broke_is_reentered_from_a_fresh_registration() {
    use crate::control_input_ack::testing;
    use aterm_agent::harness::native_act::{Succession, succession_reply};
    let (handle, mut reader) = testing::owned_handle(8135);
    let ctx = Arc::clone(&handle.ctx);
    let world = World::new(
        aterm_types::tab_id::TabId::parse(ctx.self_id.as_str())
            .expect("the owned handle has a minted tab ID"),
    )
    .unwrap();
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), Some(world.snapshot()));
    let (kernel, prepare, settled, _) = complete_cold_source(&handle, &mut reader, &world);
    let vendor = world.process().clone();
    let succession = || {
        kernel.serve(
            &ctx,
            Request::Succession(Some(settled.id.clone())),
            builtin(),
        )
    };
    let answer = |admits: Succession| succession_reply(host_epoch().unwrap(), &admits).unwrap();
    let admitted = |reentry: &Prepare| {
        Reply::parse(&kernel.serve(&ctx, Request::prepare(reentry.clone()), builtin()))
            .is_some_and(|reply| matches!(reply, Reply::Admitted(_)))
    };
    // CONTROL: the settled vendor live in the foreground is named.
    assert_eq!(succession(), answer(Succession::Predecessor));
    // CONTROL: a vendor the kernel reads as nothing, yet cannot say ended, holds the epoch, and
    // the settlement is named again once it reads.
    kernel
        .base
        .birth_observations
        .lock()
        .unwrap()
        .insert(vendor.pid, None);
    let parked = kernel.base.processes.lock().unwrap().remove(&vendor.pid);
    assert_eq!(
        succession(),
        answer(Succession::Bound("qualification-vendor-unread".into()))
    );
    kernel.base.processes.lock().unwrap().insert(
        vendor.pid,
        parked.expect("the settled vendor's fixture row"),
    );
    kernel
        .base
        .birth_observations
        .lock()
        .unwrap()
        .remove(&vendor.pid);
    assert_eq!(succession(), answer(Succession::Predecessor));
    // A registration made before the break.
    let before = register_now(
        &ctx,
        &kernel,
        &world,
        &vendor,
        &"7a".repeat(16),
        &"6".repeat(64),
    );

    // A peer claims the tab, taking every duty, Relaunch included.
    assert_eq!(
        session_timeline::claim_supervisor(
            &ctx,
            "peer-loop",
            Some(77),
            None,
            crate::metrics::now_us()
        ),
        Ok(true)
    );
    assert_eq!(
        succession(),
        answer(Succession::Bound("qualification-foreign-relaunch".into())),
        "a foreign claim on Relaunch live now holds the epoch"
    );
    let during = reentry_now(
        &ctx,
        &kernel,
        &world,
        &prepare,
        &vendor,
        before.clone(),
        &"5a".repeat(16),
    );
    assert!(
        !admitted(&during),
        "nothing re-enters under a foreign claim"
    );

    // It releases the tab: the built-in harness keeps Relaunch again, in a later generation.
    assert!(session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(
        succession(),
        answer(Succession::Reentry),
        "a settlement whose continuity broke binds nothing"
    );
    let stale = reentry_now(
        &ctx,
        &kernel,
        &world,
        &prepare,
        &vendor,
        before,
        &"5b".repeat(16),
    );
    assert!(
        !admitted(&stale),
        "a registration made before the break revives nothing"
    );
    let fresh = register_now(
        &ctx,
        &kernel,
        &world,
        &vendor,
        &"7b".repeat(16),
        &"7".repeat(64),
    );
    let reentry = reentry_now(
        &ctx,
        &kernel,
        &world,
        &prepare,
        &vendor,
        fresh,
        &"5c".repeat(16),
    );
    assert!(
        admitted(&reentry),
        "the host's own registration under the current generation re-enters"
    );
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), None);
}

/// A SETTLEMENT WHOSE CONVERSATION MOVED ON IS RE-ENTERED, NEVER HELD (a person's `/clear`, or a
/// `/resume` of another coordinated conversation, inside the settled Claude Code: the settled
/// conversation was no longer current while its vendor lived, so the host kept the settlement and
/// answered `bound` to the record now current, whose predecessor is not that settlement, for the
/// vendor's whole life): another conversation current in the settled vendor is one no later read
/// makes the settlement's again, so the epoch binds nothing, the answer is `reentry`, and a
/// conversation made current again revives nothing; the host's own registration re-enters.
///
/// NEGATIVE CONTROL: with the settlement kept while its vendor lives (`settle_current` releasing
/// only a vendor positively gone), the answer is `bound` (`qualification-conversation`).
#[test]
fn a_settlement_whose_conversation_moved_on_is_reentered_never_held() {
    use crate::control_input_ack::testing;
    use aterm_agent::harness::native_act::{Succession, succession_reply};
    let (handle, mut reader) = testing::owned_handle(8136);
    let ctx = Arc::clone(&handle.ctx);
    let world = World::new(
        aterm_types::tab_id::TabId::parse(ctx.self_id.as_str())
            .expect("the owned handle has a minted tab ID"),
    )
    .unwrap();
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), Some(world.snapshot()));
    let (kernel, prepare, settled, _) = complete_cold_source(&handle, &mut reader, &world);
    let vendor = world.process().clone();
    let succession = || {
        kernel.serve(
            &ctx,
            Request::Succession(Some(settled.id.clone())),
            builtin(),
        )
    };
    let answer = |admits: Succession| succession_reply(host_epoch().unwrap(), &admits).unwrap();
    // CONTROL: the settled conversation current is named.
    assert_eq!(succession(), answer(Succession::Predecessor));
    // The host reads another conversation as current in the settled vendor.
    kernel
        .base
        .conversation_valid
        .store(false, Ordering::SeqCst);
    assert_eq!(succession(), answer(Succession::Reentry));
    // The conversation current again (the re-entry's own) revives nothing.
    kernel.base.conversation_valid.store(true, Ordering::SeqCst);
    assert_eq!(succession(), answer(Succession::Reentry));
    let observation = register_now(
        &ctx,
        &kernel,
        &world,
        &vendor,
        &"7c".repeat(16),
        &"8".repeat(64),
    );
    let reentry = reentry_now(
        &ctx,
        &kernel,
        &world,
        &prepare,
        &vendor,
        observation,
        &"5e".repeat(16),
    );
    let reply = kernel.serve(&ctx, Request::prepare(reentry.clone()), builtin());
    assert_eq!(admitted_binding(&reply).id, reentry.id, "{reply}");
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), None);
}

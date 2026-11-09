// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_expired_memory_publication_with_a_lost_ack_keeps_its_debt() {
    use crate::control_input_ack::testing;
    let (handle, mut reader) = testing::owned_handle(8131);
    let ctx = Arc::clone(&handle.ctx);
    let mut world = World::new(ctx.self_id.as_str()).unwrap();
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), Some(world.snapshot()));
    let (mut facts, _, _, _) = complete_cold_source(&handle, &mut reader, &world);
    facts.system_clock = false;
    facts
        .base
        .prepare_journal_ack
        .store(false, Ordering::SeqCst);
    let kernel = Arc::new(facts);
    world.controls().revision(
        ctx.meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us())
            .revision(),
    );
    let prepared = Arc::new(Mutex::new(None::<Prepare>));
    let authorizer = |checked: &CheckedRelaunch| {
        let scope = crate::native_act::authorize_checked_relaunch(&ctx, checked)?;
        if checked.step() == CheckedRelaunchStep::Prepare {
            *prepared.lock().unwrap() = Some(checked.request().clone());
        }
        Ok(scope)
    };
    let native_ctx = Arc::clone(&ctx);
    let native_kernel = Arc::clone(&kernel);
    let _unknown = world.run(
        &Restart::Memory,
        &RestartAsk::of(&Restart::Memory, 32),
        &authorizer,
        move |line| {
            line.split_once(" native-act ")
                .and_then(|(_, wire)| Request::parse(wire))
                .map_or_else(
                    || "ERR fixture native grammar\n".into(),
                    |request| {
                        if matches!(&request, Request::Prepare(_)) {
                            native_kernel.base.clock.store(
                                aterm_agent::harness::native_act::monotonic_us().unwrap(),
                                Ordering::SeqCst,
                            );
                        }
                        native_kernel.serve(&native_ctx, request, builtin())
                    },
                )
        },
    );
    let request = prepared.lock().unwrap().clone();
    let evidence = request.as_ref().map(|request| {
        kernel
            .base
            .clock
            .store(request.deadline_us, Ordering::SeqCst);
        let retired = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .reclaim(request.deadline_us);
        drop(retired);
        let refusal = kernel.serve(&ctx, Request::prepare(request.clone()), builtin());
        let table = ctx.fenced_writes.native.lock().unwrap();
        let queued = table.by_id.get(&request.id).is_some_and(|operation| {
            let operation = operation.lock().unwrap();
            operation.phase == Phase::Queued && !operation.preparation.recorded
        });
        (refusal, queued, kernel.recorded_operation(&request.id))
    });
    let no_signal = kernel.base.signals.load(Ordering::SeqCst) == 0;
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
    let (refusal, queued, history) = evidence.expect("actual checked Prepare was captured");
    // The queued retry reaches the still-unacknowledged append before its deadline
    // check. That uncertainty is not the absence proof needed to clear client debt.
    assert_eq!(refusal, UNKNOWN, "the lost ACK must remain exactly Unknown");
    assert!(
        queued,
        "the unrecorded queued publication must remain retained"
    );
    assert_eq!(history, Ok(true));
    assert!(no_signal);
}

#[test]
fn an_expired_unpublished_memory_recipe_keeps_its_predecessor_and_requalifies() {
    use crate::control_input_ack::testing;
    let (handle, mut reader) = testing::owned_handle(8130);
    let ctx = Arc::clone(&handle.ctx);
    let mut world = World::new(ctx.self_id.as_str()).unwrap();
    crate::harness_host::publish_snapshot_for_test(ctx.self_id.as_str(), Some(world.snapshot()));
    let (mut facts, predecessor, _, _) = complete_cold_source(&handle, &mut reader, &world);
    facts.system_clock = false;
    let controls = world.controls();
    controls.revision(
        ctx.meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us())
            .revision(),
    );
    let (entered, arrival) = std::sync::mpsc::sync_channel(1);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    facts.base.prepare_gate = Some((entered, Arc::clone(&gate)));
    let kernel = Arc::new(facts);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let first = Arc::new(Mutex::new(None::<Prepare>));
    let exchanges = Arc::new(Mutex::new(Vec::new()));
    let native = || {
        let ctx = Arc::clone(&ctx);
        let kernel = Arc::clone(&kernel);
        let first = Arc::clone(&first);
        let exchanges = Arc::clone(&exchanges);
        move |line: &str| {
            let Some(request) = line
                .split_once(" native-act ")
                .and_then(|(_, wire)| Request::parse(wire))
            else {
                return "ERR fixture native grammar\n".into();
            };
            if let Request::Prepare(prepare) = &request {
                let mut saved = first.lock().unwrap();
                if saved.as_ref().is_none_or(|old| old.id != prepare.id) {
                    // Only a genuinely new recipe gets the real clock domain. An exact
                    // replay retains the original expiry injected at the publication gate.
                    kernel.base.clock.store(
                        aterm_agent::harness::native_act::monotonic_us().unwrap(),
                        Ordering::SeqCst,
                    );
                }
                if saved.is_none() {
                    *saved = Some((**prepare).clone());
                }
            }
            let reply = kernel.serve(&ctx, request.clone(), builtin());
            exchanges.lock().unwrap().push((request, reply.clone()));
            reply
        }
    };
    let (finished, completion) = std::sync::mpsc::channel();
    let worker = {
        let ctx = Arc::clone(&ctx);
        let calls = Arc::clone(&calls);
        let delegate = native();
        std::thread::spawn(move || {
            let authorizer = |checked: &CheckedRelaunch| {
                let scope = crate::native_act::authorize_checked_relaunch(&ctx, checked)?;
                calls
                    .lock()
                    .unwrap()
                    .push((checked.step(), checked.request().clone()));
                Ok(scope)
            };
            let _answer = world.run(
                &Restart::Memory,
                &RestartAsk::of(&Restart::Memory, 31),
                &authorizer,
                delegate,
            );
            let _ = finished.send(());
            world
        })
    };
    let arrived = arrival.recv_timeout(PATIENCE);
    let old = first.lock().unwrap().clone();
    let reserved_only = old.as_ref().is_some_and(|old| {
        let table = ctx.fenced_writes.native.lock().unwrap();
        table.reservations.contains_key(&old.id) && !table.by_id.contains_key(&old.id)
    });
    if let Some(old) = &old {
        kernel.base.clock.store(old.deadline_us, Ordering::SeqCst);
    }
    let reserved_refused = old.as_ref().is_some_and(|old| {
        kernel.serve(&ctx, Request::prepare(old.clone()), builtin()) == DEADLINE
    });
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    // Release precedes every assertion and the completion detector precedes joining.
    completion
        .recv_timeout(PATIENCE)
        .expect("expired preparation worker did not finish");
    let mut world = worker.join().unwrap();
    let unpublished = old.as_ref().is_some_and(|old| {
        let table = ctx.fenced_writes.native.lock().unwrap();
        !table.reservations.contains_key(&old.id)
            && !table.by_id.contains_key(&old.id)
            && matches!(
                table.asked_binding,
                asked_upgrade::CurrentBinding::Settled(_)
            )
            && kernel.recorded_operation(&old.id) == Ok(false)
    });
    let no_signal = kernel.base.signals.load(Ordering::SeqCst) == 0;
    let before_cleanup = calls.lock().unwrap().len();
    let authorizer = |checked: &CheckedRelaunch| {
        let scope = crate::native_act::authorize_checked_relaunch(&ctx, checked)?;
        calls
            .lock()
            .unwrap()
            .push((checked.step(), checked.request().clone()));
        Ok(scope)
    };
    controls.background(&["owned work"]);
    let _cleanup = world.run(
        &Restart::Memory,
        &RestartAsk::of(&Restart::Memory, 31),
        &authorizer,
        native(),
    );
    let cleanup_read_only = calls.lock().unwrap().len() == before_cleanup;
    let _veto = world.run(
        &Restart::Memory,
        &RestartAsk::of(&Restart::Memory, 31),
        &authorizer,
        native(),
    );
    let changed_work_veto = calls.lock().unwrap().len() == before_cleanup
        && kernel.base.signals.load(Ordering::SeqCst) == 0;
    controls.background(&[]);
    let _replacement = world.run(
        &Restart::Memory,
        &RestartAsk::of(&Restart::Memory, 31),
        &authorizer,
        native(),
    );
    let requests = calls.lock().unwrap().clone();
    let exchanges = exchanges.lock().unwrap().clone();
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
    assert!(arrived.is_ok() && reserved_only && reserved_refused && unpublished && no_signal);
    assert!(cleanup_read_only && changed_work_veto);
    let old = old.expect("actual first Prepare");
    assert!(
        exchanges
            .iter()
            .any(|(request, reply)| request == &Request::prepare(old.clone())
                && reply == &aterm_agent::harness::native_act::unadmitted_reply(&old.id).unwrap())
    );
    let prepares = requests
        .iter()
        .filter(|(step, _)| *step == CheckedRelaunchStep::Prepare)
        .map(|(_, request)| request)
        .collect::<Vec<_>>();
    assert_eq!(prepares.len(), 2);
    assert_ne!(prepares[1].id, old.id);
    assert_ne!(prepares[1].deadline_us, old.deadline_us);
    assert!(
        matches!(&prepares[1].asked, Some(AskedIntent::Relaunch(intent)) if intent.predecessor == predecessor.id)
    );
    assert_eq!(signals, 1);
}

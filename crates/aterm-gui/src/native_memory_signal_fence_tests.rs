// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The actual last kernel observation may cross a first-Signal scalar fence.

use super::*;

#[test]
fn first_memory_signal_rechecks_human_and_budget_after_the_last_kernel_read() {
    use crate::control_input_ack::testing;
    for case in 0..3 {
        let (handle, mut reader) = testing::owned_handle(8130 + case);
        let ctx = Arc::clone(&handle.ctx);
        let mut world = World::new(ctx.self_id.as_str()).unwrap();
        crate::harness_host::publish_snapshot_for_test(
            ctx.self_id.as_str(),
            Some(world.snapshot()),
        );
        let (mut kernel, _, _, _) = complete_cold_source(&handle, &mut reader, &world);
        // Only expiry uses an injected clock. Admission synchronizes its initial reading
        // with the real producer, then the controller advances the same retained clock.
        kernel.system_clock = case != 2;
        let kernel = Arc::new(kernel);
        let controls = world.controls();
        controls.revision(
            ctx.meta
                .lock()
                .unwrap()
                .supervisor_authority(crate::metrics::now_us())
                .revision(),
        );
        let source_pid = world.process().pid;
        let (entered, arrival) = mpsc::sync_channel(1);
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let signal_request = Arc::new(Mutex::new(None::<Prepare>));
        let (finished, completion) = mpsc::channel();
        let worker = {
            let ctx = Arc::clone(&ctx);
            let kernel = Arc::clone(&kernel);
            let gate = Arc::clone(&gate);
            let signal_request = Arc::clone(&signal_request);
            std::thread::spawn(move || {
                let authorizer = |checked: &CheckedRelaunch| {
                    let scope = crate::native_act::authorize_checked_relaunch(&ctx, checked)?;
                    if checked.step() == CheckedRelaunchStep::Signal {
                        *signal_request.lock().unwrap() = Some(checked.request().clone());
                        *kernel.signal_foreground_gate.lock().unwrap() =
                            Some((entered.clone(), Arc::clone(&gate)));
                    }
                    Ok(scope)
                };
                let native_ctx = Arc::clone(&ctx);
                let native_kernel = Arc::clone(&kernel);
                let answer = world.run(
                    &Restart::Memory,
                    &RestartAsk::of(&Restart::Memory, 23),
                    &authorizer,
                    move |line| {
                        let Some(request) = line
                            .split_once(" native-act ")
                            .and_then(|(_, wire)| Request::parse(wire))
                        else {
                            return "ERR fixture native grammar\n".into();
                        };
                        if case == 2
                            && matches!(&request, Request::Prepare(prepare)
                            if matches!(prepare.asked, Some(AskedIntent::Relaunch(_))))
                        {
                            native_kernel.base.clock.store(
                                aterm_agent::harness::native_act::monotonic_us().unwrap(),
                                Ordering::SeqCst,
                            );
                        }
                        native_kernel.serve(&native_ctx, request, builtin())
                    },
                );
                let _sent = finished.send(());
                (world, answer.report.step)
            })
        };
        let arrived = arrival.recv_timeout(PATIENCE);
        let saved = signal_request
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let no_early_signal = kernel.base.signals.load(Ordering::SeqCst) == 0;
        if arrived.is_ok() {
            match case {
                1 => {
                    ctx.human_input
                        .note(crate::metrics::now_us(), crate::human_input::Gesture::Typed);
                    controls.human_sequence(ctx.human_input.seq());
                }
                2 => {
                    if let Some(request) = &saved {
                        kernel
                            .base
                            .clock
                            .store(request.deadline_us, Ordering::SeqCst);
                    }
                }
                _ => {}
            }
        }
        // Release even if phase readiness failed. The completion detector precedes join;
        // neither a failed witness nor a failed assertion strands an owned gate.
        {
            let (lock, changed) = &*gate;
            *lock.lock().unwrap_or_else(|p| p.into_inner()) = true;
            changed.notify_all();
        }
        let completed = completion.recv_timeout(PATIENCE);
        completed.expect("first-Signal worker did not complete after gate release");
        let (mut world, step) = worker.join().unwrap();
        let signals = kernel.base.signals.load(Ordering::SeqCst);
        let closed = world.close();
        kernel.base.processes.lock().unwrap().remove(&source_pid);
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
        assert!(closed.is_ok(), "the actual producer world did not retire");
        assert!(
            arrived.is_ok(),
            "case {case}: actual final kernel read not reached: {step}"
        );
        assert!(
            saved.is_some(),
            "the real first-Signal authorization was required"
        );
        assert!(no_early_signal);
        assert_eq!(signals, u32::from(case == 0), "case {case}: {step}");
    }
}

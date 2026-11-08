// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn closed_rollback(local: u64) -> World {
    let world = world_with_retirement(local, false);
    // Seed the phase established by the existing rollback protocol. The
    // regression is capture, new admission, and retirement before another Resume.
    lookup(&world.handle.ctx, &world.old)
        .unwrap()
        .lock()
        .unwrap()
        .resume_phase = Some(ImagePhase::Rollback);
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::Reconcile(world.old.clone()),
            world.actor.clone(),
        ),
        NOEXEC
    );
    install(&world.handle.ctx, 71, &[Scope::Upgrade, Scope::Relaunch]);
    world
        .facts
        .clock
        .store(world.original.deadline_us + 1, Ordering::SeqCst);
    world
}

#[test]
fn rollback_continuation_refuses_forward_and_resignal_but_resumes_rollback() {
    let mut world = closed_rollback(8241);
    let _journal_home = use_real_journal(&mut world);
    let (root, attempt, request) = publish(&world, "e1");
    renewal::stage_continuation(&world.handle.ctx, &root, &attempt, &world.facts).unwrap();
    let binding = admitted_binding(&run(
        &world.handle.ctx,
        &world.facts,
        Request::prepare(request),
        world.actor.clone(),
    ));
    let ctx = &world.handle.ctx;
    for forbidden in [
        Request::Signal(binding.clone()),
        Request::Resume(
            binding.clone(),
            ImagePhase::Forward,
            "1.1".into(),
            ctx.human_input.seq(),
        ),
    ] {
        assert_eq!(
            run(ctx, &world.facts, forbidden, world.actor.clone()),
            DENIED
        );
    }
    let reply = serve_by(
        ctx,
        -1,
        Request::Resume(
            binding,
            ImagePhase::Rollback,
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
    assert_eq!(reply, "OK native-act state=resumed phase=rollback\n");
    assert_eq!(testing::byte(&mut world.reader), b'R');
    assert_eq!(world.facts.signals.load(Ordering::SeqCst), 1);
    assert_eq!(world.facts.grant_journal_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn rollback_latch_survives_three_children_expiring_before_their_first_resume() {
    let mut world = closed_rollback(8242);
    let _journal_home = use_real_journal(&mut world);
    let mut root = world.root.clone();
    let mut previous = world.original.clone();
    for id in ["e2", "e3", "e4"] {
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
            deadline_us: world.facts.clock.load(Ordering::SeqCst) + MAX_PREPARE_US,
            ..previous.clone()
        };
        let (updated, attempt) = recovery::with_receipt_writer(&world.env, |writer| {
            let attempt = writer.propose(&root, &previous.id, &request)?;
            let updated = writer.publish(&root, &attempt)?;
            Ok((updated, attempt))
        })
        .unwrap()
        .unwrap();
        renewal::stage_continuation(&world.handle.ctx, &updated, &attempt, &world.facts).unwrap();
        let binding = admitted_binding(&run(
            &world.handle.ctx,
            &world.facts,
            Request::prepare(request.clone()),
            world.actor.clone(),
        ));
        let operation = lookup(&world.handle.ctx, &binding).unwrap();
        assert_eq!(
            operation.lock().unwrap().resume_phase,
            Some(ImagePhase::Rollback)
        );
        world
            .facts
            .clock
            .store(request.deadline_us + 1, Ordering::SeqCst);
        assert_eq!(
            run(
                &world.handle.ctx,
                &world.facts,
                Request::Reconcile(binding.clone()),
                world.actor.clone(),
            ),
            NOEXEC
        );
        let op = operation.lock().unwrap();
        assert!(!op.resume_sent);
        assert_eq!(
            op.closed_restart.as_ref().unwrap().initial_resume_phase(),
            Some(ImagePhase::Rollback)
        );
        drop(op);
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
        previous = request;
        root = updated;
    }
    assert_eq!(world.facts.signals.load(Ordering::SeqCst), 1);
    assert_eq!(world.facts.grant_journal_calls.load(Ordering::SeqCst), 0);
}

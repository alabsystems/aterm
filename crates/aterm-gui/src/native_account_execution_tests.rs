// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::control_input_ack::{InputMark, testing};
use crate::input::{Delivery, EgressReceipt};

#[test]
fn account_refusal_never_grants_exec_and_only_dead_settled_helpers_can_retry() {
    let (handle, mut reader) = testing::owned_handle(7953);
    let ctx = &handle.ctx;
    install(ctx, 71, &[Scope::Upgrade]);
    let facts = Facts::new();
    let old = process(4242, 400, 4242, 400, 2);
    let shell = process(400, 1, 400, 400, 4);
    facts.processes.lock().unwrap().insert(old.pid, old.clone());
    facts
        .processes
        .lock()
        .unwrap()
        .insert(shell.pid, shell.clone());
    let mut request = plan(ctx, &facts);
    request.holder = crate::harness_host::holder();
    request.expected_human_seq = Some(ctx.human_input.seq());
    request.duty = Scope::Relaunch;
    request.plan = NativePlan::Restart {
        cold: None,
        observation: None,
        process: old,
        shell,
        launcher: image(20),
        socket: "/fake-native.sock".into(),
        forward: Box::new(restart_command(9)),
        rollback: None,
    };
    let builtin = SupervisorInputCaller {
        principal: Principal::Harness,
        connection: Some(8),
        peer: Some(900),
        proof: None,
    };
    let binding = admitted_binding(&run(
        ctx,
        &facts,
        Request::prepare(request),
        builtin.clone(),
    ));
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Signal(binding.clone()),
            builtin.clone()
        ),
        "OK native-act state=signalled\n"
    );
    facts.processes.lock().unwrap().remove(&4242);
    facts.foreground.store(400, Ordering::SeqCst);
    let mut dispatch = |_: &str| {
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
    };
    let resume = || {
        Request::Resume(
            binding.clone(),
            ImagePhase::Forward,
            "1.1".into(),
            ctx.human_input.seq(),
        )
    };
    assert_eq!(
        serve_by(ctx, -1, resume(), builtin.clone(), &facts, &mut dispatch),
        "OK native-act state=resumed phase=forward\n"
    );
    assert_eq!(testing::byte(&mut reader), b'R');
    let launcher = SupervisorInputCaller {
        principal: Principal::Instance,
        connection: Some(80),
        peer: Some(600),
        proof: None,
    };
    facts
        .processes
        .lock()
        .unwrap()
        .insert(600, process(600, 400, 600, 400, 20));
    facts.foreground.store(600, Ordering::SeqCst);
    assert!(matches!(
        Reply::parse(&run(
            ctx,
            &facts,
            Request::Attach(binding.clone(), ImagePhase::Forward),
            launcher.clone()
        )),
        Some(Reply::Launch { .. })
    ));
    facts.account_allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Execute(binding.clone(), ImagePhase::Forward),
            launcher.clone()
        ),
        DENIED
    );
    let operation = lookup(ctx, &binding).unwrap();
    assert!(
        !operation.lock().unwrap().children[0]
            .as_ref()
            .unwrap()
            .execute_sent
    );
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Query(binding.clone()),
            builtin.clone()
        ),
        UNKNOWN,
        "a live helper is not unissued proof"
    );
    facts.processes.lock().unwrap().remove(&600);
    facts.foreground.store(400, Ordering::SeqCst);
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Query(binding.clone()),
            builtin.clone()
        ),
        "OK native-act state=unissued phase=forward\n"
    );
    facts
        .processes
        .lock()
        .unwrap()
        .insert(601, process(601, 400, 600, 400, 91));
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Query(binding.clone()),
            builtin.clone()
        ),
        UNKNOWN,
        "a surviving group prevents retry"
    );
    facts.processes.lock().unwrap().remove(&601);
    facts.account_allowed.store(true, Ordering::SeqCst);
    assert_eq!(
        serve_by(ctx, -1, resume(), builtin.clone(), &facts, &mut dispatch),
        "OK native-act state=resumed phase=forward\n"
    );
    assert_eq!(testing::byte(&mut reader), b'R');
    facts
        .processes
        .lock()
        .unwrap()
        .insert(600, process(600, 400, 600, 400, 20));
    facts.foreground.store(600, Ordering::SeqCst);
    assert!(matches!(
        Reply::parse(&run(
            ctx,
            &facts,
            Request::Attach(binding.clone(), ImagePhase::Forward),
            launcher.clone()
        )),
        Some(Reply::Launch { .. })
    ));
    assert!(matches!(
        Reply::parse(&run(
            ctx,
            &facts,
            Request::Execute(binding.clone(), ImagePhase::Forward),
            launcher.clone()
        )),
        Some(Reply::Execute { .. })
    ));
    facts.account_allowed.store(false, Ordering::SeqCst);
    assert!(
        matches!(
            Reply::parse(&run(
                ctx,
                &facts,
                Request::Execute(binding.clone(), ImagePhase::Forward),
                launcher
            )),
            Some(Reply::Execute { .. })
        ),
        "a granted exec has an idempotent receipt"
    );
    facts.processes.lock().unwrap().remove(&600);
    assert_eq!(
        run(ctx, &facts, Request::Query(binding), builtin),
        UNKNOWN,
        "granted work never becomes unissued"
    );
    assert_eq!(facts.signals.load(Ordering::SeqCst), 1);
}

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

/// A forward command on the account's vault token: aterm's wrapper (`image`, inode 9), which
/// execs the store build it names (`settles`, inode 11) in the same pid
/// (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.4).
fn token_command() -> RestartCommand {
    use aterm_agent::harness::{accounts, accounts_pool::ExecutionIntent};
    RestartCommand {
        settles: Some(image(11)),
        account_intent: Some(Box::new(ExecutionIntent::Token {
            namespace: "ab".repeat(32),
            login: None,
            owner: Some("work".into()),
            entry: accounts::TokenEntry::parse("aterm-claude-token-ac-0123456789abcdef").unwrap(),
        })),
        program: "/image-9".into(),
        argv: [
            "harness",
            "accounts",
            "token-run",
            "work",
            "--",
            "/image-11",
            "--resume",
            "captured-conversation",
        ]
        .map(String::from)
        .to_vec(),
        ..restart_command(9)
    }
}

/// THE SEAT ADMITS A VAULT TOKEN: a stored login and a token intent go to
/// `accounts_pool::admit_execution`, which holds a token to its custody and its standing here; an
/// external route is admitted apart. CONTROL: an external intent is not the seat's.
#[test]
fn the_seat_admits_a_stored_login_and_a_vault_token() {
    use aterm_agent::harness::accounts_pool::ExecutionIntent;
    let Some(token) = token_command().account_intent else {
        panic!("a token intent");
    };
    assert!(seat_admitted(&token));
    let stored = ExecutionIntent::Stored {
        namespace: "ab".repeat(32),
        login: None,
        owner: Some("work".into()),
        generation: "cd".repeat(32),
    };
    assert!(seat_admitted(&stored));
    assert!(!seat_admitted(&ExecutionIntent::external()));
}

/// A TOKEN COMMAND RUNS THROUGH THE NATIVE HOST AND SETTLES ON ITS BUILD: Execute grants the
/// wrapper's exec, and Precommit and Commit hold the child to the build the wrapper execs in its
/// own pid, never to the wrapper. CONTROLS: a child still on the wrapper, or on any other image, is
/// UNKNOWN, and nothing is committed until it maps the build.
#[test]
fn a_token_command_executes_and_settles_on_the_build_its_wrapper_execs() {
    let (handle, mut reader) = testing::owned_handle(7954);
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
    let forward = token_command();
    assert_eq!(
        RestartCommand::parse(&forward.encode().unwrap()).as_ref(),
        Some(&forward),
        "a token command's settled image round-trips"
    );
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
        forward: Box::new(forward),
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
    assert_eq!(
        serve_by(
            ctx,
            -1,
            Request::Resume(
                binding.clone(),
                ImagePhase::Forward,
                "1.1".into(),
                ctx.human_input.seq(),
            ),
            builtin.clone(),
            &facts,
            &mut dispatch
        ),
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
    assert!(matches!(
        Reply::parse(&run(
            ctx,
            &facts,
            Request::Execute(binding.clone(), ImagePhase::Forward),
            launcher
        )),
        Some(Reply::Execute { .. })
    ));
    let precommit = || {
        run(
            ctx,
            &facts,
            Request::Precommit(binding.clone(), ImagePhase::Forward),
            builtin.clone(),
        )
    };
    let maps = |inode| facts.processes.lock().unwrap().get_mut(&600).unwrap().image = image(inode);
    for (inode, what) in [
        (9, "the wrapper, reading the vault"),
        (12, "another program"),
    ] {
        maps(inode);
        assert_eq!(precommit(), UNKNOWN, "a child on {what} has not settled");
    }
    maps(11);
    assert_eq!(precommit(), "OK native-act state=precommitted\n");
    let committed = run(
        ctx,
        &facts,
        Request::Commit(binding, ImagePhase::Forward, "1.0.70".into()),
        builtin,
    );
    let Some(Reply::RestartCommitted(observed)) = Reply::parse(&committed) else {
        panic!("a token command commits on its build: {committed}");
    };
    assert_eq!(
        observed.registration.vendor,
        process(600, 400, 600, 400, 11)
    );
}

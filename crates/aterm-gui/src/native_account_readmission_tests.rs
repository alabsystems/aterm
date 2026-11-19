// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A native restart's Execute readmits a stored login whose credential generation moved after
//! its signal, and its completion names the intent it admitted.

use super::*;
use crate::control_input_ack::{InputMark, testing};
use crate::input::{Delivery, EgressReceipt};
use aterm_agent::harness::accounts_pool::{ExecutionIntent, Purpose};

fn stored(generation: &str) -> ExecutionIntent {
    ExecutionIntent::Stored {
        namespace: "ab".repeat(32),
        login: Some("/logins/work-2".into()),
        owner: Some("work-2".into()),
        generation: generation.repeat(32),
        purpose: Purpose::SameLogin,
    }
}

/// What a granted, committed native restart of a forward command carrying `planned` keeps and
/// answers, its Execute's account admission readmitting `readmitted` (`None`: the command's own
/// intent): the commit reply, the intent the operation recorded for the forward phase, and the
/// command's own intent read back from the operation.
fn executed(
    local: u64,
    planned: &ExecutionIntent,
    readmitted: Option<ExecutionIntent>,
) -> (String, Option<ExecutionIntent>, Option<ExecutionIntent>) {
    let (handle, mut reader) = testing::owned_handle(local);
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
        forward: Box::new(RestartCommand {
            account_intent: Some(Box::new(planned.clone())),
            ..restart_command(9)
        }),
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
    *facts.account_readmitted.lock().unwrap() = readmitted;
    assert!(matches!(
        Reply::parse(&run(
            ctx,
            &facts,
            Request::Execute(binding.clone(), ImagePhase::Forward),
            launcher
        )),
        Some(Reply::Execute { .. })
    ));
    let (kept, command) = {
        let operation = lookup(ctx, &binding).unwrap();
        let op = operation.lock().unwrap();
        (
            op.readmitted[0].clone(),
            command_for(&op, ImagePhase::Forward)
                .and_then(|command| command.account_intent.as_deref())
                .cloned(),
        )
    };
    facts.processes.lock().unwrap().get_mut(&600).unwrap().image = image(9);
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Precommit(binding.clone(), ImagePhase::Forward),
            builtin.clone(),
        ),
        "OK native-act state=precommitted\n"
    );
    let committed = run(
        ctx,
        &facts,
        Request::Commit(binding, ImagePhase::Forward, "1.0.70".into()),
        builtin,
    );
    (committed, kept, command)
}

/// AN EXECUTE THAT READMITS A RENEWED LOGIN RECORDS IT AND ITS COMPLETION NAMES IT (a login
/// renewed between a native restart's signal and its Execute, which no plan can be made again
/// for): the account seat admits the same login captured afresh at Execute, the exec is granted,
/// the operation records that intent for the phase beside its command (which keeps the planned
/// intent, the recipe's identity), and the committed reply names it, `account-intent=` beside the new
/// child's observation, for the harness's record to keep. CONTROL: an Execute that admits its
/// command's own intent records none, and its completion is the plain observed one.
///
/// NEGATIVE CONTROLS (2026-11-13, each restored with cp from a backup, confirmed with cmp): with
/// the readmitted intent never recorded, nothing was kept; with the completion naming
/// only a cold restart's intent, the reply was the plain observed one.
#[test]
fn an_execute_that_readmits_a_renewed_login_records_it_and_its_completion_names_it() {
    let planned = stored("cd");
    let renewed = stored("ef");
    let (committed, kept, command) = executed(7955, &planned, Some(renewed.clone()));
    assert_eq!(kept.as_ref(), Some(&renewed));
    assert_eq!(
        command.as_ref(),
        Some(&planned),
        "the command keeps its planned intent"
    );
    match Reply::parse(&committed) {
        Some(Reply::AdmittedRestartCommitted {
            claim,
            observation,
            intent,
        }) => {
            assert_eq!(claim, ClaimOutcome::Builtin);
            assert!(observation.is_some(), "the new child stays observed");
            assert_eq!(*intent, renewed);
        }
        other => panic!("the completion names the readmitted intent: {committed} {other:?}"),
    }

    let (committed, kept, command) = executed(7956, &planned, None);
    assert_eq!(kept, None);
    assert_eq!(command.as_ref(), Some(&planned));
    assert!(
        matches!(Reply::parse(&committed), Some(Reply::RestartCommitted(_))),
        "{committed}"
    );
}

/// THE EXECUTE'S SEAT ADMISSION READMITS A RENEWED LOGIN WHERE THE SIGNAL'S STAYS EXACT (the
/// production arm, [`seat_admission`], on a real account store): `m16`, the default home's row
/// with no binding of its own, has its credential generation moved by `work-2`'s renewal. Its
/// same-login and selection intents are each refused before a signal (CONTROL: exact) and
/// readmitted at Execute as the same login captured afresh, which the exact admission then takes.
/// CONTROLS at Execute: a selection intent is held to its own selection policy (`m16` recorded
/// signed out refuses it while the same-login intent is readmitted), an external route is not the
/// seat's and is never readmitted (Execute admits it by its own external admission, as before the
/// signal), and a login filed under another row is never readmitted.
///
/// NEGATIVE CONTROL (2026-11-13, restored with cp from a backup, confirmed with cmp): with Execute
/// admitting exactly, the renewed same-login intent was refused.
#[test]
fn the_executes_seat_admission_readmits_a_renewed_login_where_the_signals_stays_exact() {
    use aterm_agent::harness::{accounts, accounts_pool};
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let state = dir.path().join("state");
    let config = dir.path().join("config");
    let work = dir.path().join("logins").join("work-2");
    let claude = accounts::Cli::Claude;
    std::fs::create_dir_all(home.join(claude.accounts().default_home)).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    let roster = |m16: &str| {
        std::fs::write(
            config.join(claude.accounts().roster_file),
            format!(
                "[[account]]\nlabel = \"{m16}\"\nclass = \"dedicated\"\n\n[[account]]\nlabel = \
                 \"work-2\"\nclass = \"dedicated\"\ndir = '{}'\n",
                work.display()
            ),
        )
        .unwrap();
    };
    roster("m16");
    let now = aterm_time::unix_secs()
        .and_then(|now| i64::try_from(now).ok())
        .unwrap();
    let seat = accounts_pool::Seat {
        config_dir: &config,
        state: &state,
        home: &home,
        now,
    };
    seat.store(claude)
        .transact(now, &mut None, |state| {
            state.home_accounts.extend(["m16".into(), "work-2".into()]);
            Ok(Vec::new())
        })
        .unwrap();
    let who = accounts::ProcessLogin {
        login: None,
        default_home: true,
        route: accounts::AuthRoute::Stored,
    };
    let same = accounts_pool::capture_execution(&seat, claude, &who, Purpose::SameLogin).unwrap();
    let select = accounts_pool::capture_execution(&seat, claude, &who, Purpose::Select).unwrap();
    for intent in [&same, &select] {
        for at in [AccountAt::Signal, AccountAt::Execute] {
            let (guard, readmitted) = seat_admission(&seat, intent, at).unwrap();
            assert!(guard.is_some(), "the writer is held through the grant");
            assert_eq!(readmitted, None, "an unchanged intent admits as itself");
        }
    }
    let renewed_dir = work.to_str().unwrap();
    seat.store(claude)
        .transact_logins(now, &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "work-2",
                Some(renewed_dir),
                renewed_dir,
                None,
                now,
            ))
        })
        .unwrap();
    for intent in [&same, &select] {
        assert!(
            seat_admission(&seat, intent, AccountAt::Signal).is_err(),
            "CONTROL: the generation moved, and before a signal the intent is admitted exactly"
        );
        let (guard, readmitted) = seat_admission(&seat, intent, AccountAt::Execute)
            .expect("Execute readmits the same login");
        drop(guard);
        let readmitted = readmitted.expect("the login captured afresh");
        assert_ne!(&readmitted, intent, "a new generation");
        assert!(readmitted.same_login(intent), "{readmitted:?} {intent:?}");
        assert!(seat_admission(&seat, &readmitted, AccountAt::Signal).is_ok());
    }
    seat.store(claude)
        .transact(now, &mut None, |state| {
            accounts::note_auth(state, "m16", accounts::Auth::SignedOut, now);
            Ok(Vec::new())
        })
        .unwrap();
    assert!(
        seat_admission(&seat, &select, AccountAt::Execute).is_err(),
        "a selection intent is not readmitted past its own selection policy"
    );
    assert!(seat_admission(&seat, &same, AccountAt::Execute).is_ok());
    assert!(seat_admission(&seat, &ExecutionIntent::external(), AccountAt::Execute).is_err());
    roster("m16-b");
    assert!(
        seat_admission(&seat, &same, AccountAt::Execute).is_err(),
        "a login filed under another row is never readmitted"
    );
}

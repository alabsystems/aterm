// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::fd::AsRawFd;

#[test]
fn native_channel_routes_source_actions_without_a_read_only_prepare() {
    use aterm_types::control_verbs::{OpClass, spec_for};
    let handle = crate::session_store::test_handle_on(918043, -1);
    let ctx = &handle.ctx;
    let (server, _client) = aterm_uds::CtlStream::pair().unwrap();
    let _direct = crate::control::DirectPeer::enter();
    let _serving = aterm_control::hangup::ServingFd::enter(server.as_raw_fd());
    for (line, read) in [
        ("info source-exec", true),
        ("prepare source-exec", false),
        ("query source-exec", false),
        ("seal-input source-exec", false),
        ("cancel-prepare source-exec", false),
        ("settle source-exec", false),
        ("adopt source-exec", false),
    ] {
        assert_eq!(
            spec_for("native-act", line).unwrap().op == OpClass::Read,
            read
        );
    }
    let mut resume = |_: &str| panic!("source preparation cannot resume work");
    assert!(
        super::super::serve(ctx, -1, "info source-exec", &mut resume)
            .contains("execution=unavailable")
    );
    for line in [
        "prepare source-exec operation=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa build=1 source-build=1 primary=00000000-0000-4000-8000-000000000001",
        "info source-exec prepare operation=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "execute source-exec operation=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "settle source-exec operation=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "adopt source-exec operation=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        assert_eq!(super::super::serve(ctx, -1, line, &mut resume), REFUSED);
    }
    assert!(
        ctx.fenced_writes
            .native
            .lock()
            .unwrap()
            .source_exec
            .is_none()
    );
}

#[test]
fn a_kernel_peer_without_the_real_foreground_claim_allocates_nothing() {
    let handle = crate::session_store::test_handle_on(918039, -1);
    let ctx = &handle.ctx;
    let (server, _client) = aterm_uds::CtlStream::pair().unwrap();
    let _direct = crate::control::DirectPeer::enter();
    let _serving = aterm_control::hangup::ServingFd::enter(server.as_raw_fd());
    assert!(serve(ctx, -1, "info").contains("execution=unavailable"));
    let request = "prepare operation=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa build=1 source-build=1 primary=00000000-0000-4000-8000-000000000001";
    assert_eq!(serve(ctx, -1, request), REFUSED);
    assert_eq!(
        serve(
            ctx,
            -1,
            "execute operation=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ),
        REFUSED
    );
    let table = ctx.fenced_writes.native.lock().unwrap();
    assert!(table.source_exec.is_none());
    assert!(table.runtime_data.is_none());
}

#[test]
fn preparation_recipe_refuses_ambiguous_fields_and_noncanonical_builds() {
    let canonical = "prepare operation=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa build=1 source-build=1 primary=00000000-0000-4000-8000-000000000001";
    let (_, values) = fields(canonical).unwrap();
    assert!(recipe(&values).is_some());
    for line in [
        canonical.replace("build=1", "build=01"),
        canonical.replace("build=1", "build=0"),
        canonical.replace("build=1", "build=1 build=2"),
        canonical.replace(
            "00000000-0000-4000-8000-000000000001",
            "00000000-0000-0000-0000-000000000000",
        ),
        format!("{canonical} permit=true"),
    ] {
        assert!(
            fields(&line)
                .and_then(|(_, fields)| recipe(&fields))
                .is_none()
        );
    }
}

#[cfg(target_os = "macos")]
fn resource_root(
    ctx: &Arc<crate::SessionCtx>,
    operation: &str,
) -> (Arc<Operation>, crate::session_timeline::NativeDrain) {
    resource_root_of(
        ctx,
        operation,
        Process::observe(std::process::id()).unwrap(),
        aterm_agent::harness::native_act::monotonic_us().unwrap() + 60_000_000,
    )
}

#[cfg(target_os = "macos")]
fn resource_root_of(
    ctx: &Arc<crate::SessionCtx>,
    operation: &str,
    source: Process,
    deadline: u64,
) -> (Arc<Operation>, crate::session_timeline::NativeDrain) {
    let data = runtime_data::Slot::reserved(operation.into(), source.clone());
    let pin = data.pin(operation, &source).unwrap();
    let meta = ctx.meta.lock().unwrap();
    let authority = meta.supervisor_authority(crate::metrics::now_us());
    static BOUNDARY: aterm_types::update_boundary::Boundary =
        aterm_types::update_boundary::Boundary::new();
    let boundary = BOUNDARY
        .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
        .unwrap();
    let custody = ctx
        .fenced_writes
        .reserve_native(authority.revision(), operation, boundary)
        .unwrap();
    assert!(custody.predecessors_drained());
    let drain = custody.drain();
    let op = Arc::new(Operation {
        recipe: Recipe {
            operation: operation.into(),
            build: 1,
            source_build: 1,
            primary: "00000000-0000-4000-8000-000000000001".into(),
        },
        generation: generation(source.pid).unwrap(),
        source,
        authority,
        human_seq: ctx.human_input.seq(),
        input_sealed: std::sync::atomic::AtomicBool::new(false),
        deadline,
        cancelled: std::sync::atomic::AtomicBool::new(false),
        state: Mutex::new(State {
            phase: Phase::Preparing,
            custody: Some(Arc::new(custody)),
            data: Some(pin),
            requested: false,
            possible: false,
            granted: false,
            snapshot: None,
            successor: None,
            kept_claim: None,
        }),
        wake: std::sync::Condvar::new(),
    });
    {
        let mut table = ctx.fenced_writes.native.lock().unwrap();
        table.runtime_data = Some(data);
        table.source_exec = Some(op.clone());
    }
    (op, drain)
}

#[cfg(target_os = "macos")]
#[test]
fn explicit_abort_discharges_real_input_and_data_custody_without_go() {
    let handle = crate::session_store::test_handle_on(918040, -1);
    let ctx = &handle.ctx;
    let (server, _client) = aterm_uds::CtlStream::pair().unwrap();
    let _direct = crate::control::DirectPeer::enter();
    let _serving = aterm_control::hangup::ServingFd::enter(server.as_raw_fd());
    let operation = "b".repeat(32);
    // Resource root injection only: no target lease, SDK constructor or Go is
    // created. The kernel peer, real input marker and data pin are exercised.
    let (op, drain) = resource_root(ctx, &operation);
    assert!(pending(ctx));
    assert!(!drain.is_drained());
    assert!(
        runtime_data::serve(ctx, -1, &format!("release operation={operation}"))
            .contains("state=busy")
    );
    assert!(serve(ctx, -1, &format!("abort operation={operation}")).contains("state=closed"));
    assert!(!pending(ctx));
    assert!(drain.is_drained());
    op.finish(Phase::Failed);
    assert!(
        serve(ctx, -1, &format!("query operation={operation}")).contains("state=closed"),
        "late worker failure cannot rewrite closure"
    );
    assert!(serve(ctx, -1, &format!("abort operation={operation}")).contains("state=closed"));
    assert!(
        runtime_data::serve(ctx, -1, &format!("release operation={operation}"))
            .contains("state=released")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn worker_panic_and_metadata_side_cancellation_leave_no_input_custody() {
    for panic in [false, true] {
        let handle = crate::session_store::test_handle_on(918041 + u64::from(panic), -1);
        let ctx = &handle.ctx;
        let (op, drain) = resource_root(ctx, &"c".repeat(32));
        if panic {
            guarded(&op, || panic!("source preparation fixture"));
        } else {
            let seen = Seen::of(ctx);
            assert!(cancel_owner(ctx));
            assert!(pending(ctx), "a metadata-side stop only requests closing");
            assert!(!op.current(ctx, &seen));
            drop(seen);
            guarded(&op, || op.close());
        }
        assert!(!pending(ctx));
        assert!(drain.is_drained());
        assert!(op.status().contains("state=closed"));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn possible_execution_keeps_real_custody_after_abort_panic_and_claim_eof() {
    let handle = crate::session_store::test_handle_on(918044, -1);
    let ctx = &handle.ctx;
    crate::session_timeline::claim_supervisor_as(
        ctx,
        &format!("codex-alab@{}", std::process::id()),
        Some(51),
        None,
        Some(crate::self_claim::Proven {
            pid: std::process::id(),
            start: "resource-fixture".into(),
        }),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .unwrap();
    // Resource fixture only, no constructor or signed target is injected.
    let (op, drain) = resource_root(ctx, &"d".repeat(32));
    assert!(!super::super::retain_exec_claim(ctx, 918044, 51));
    op.state.lock().unwrap().possible = true;
    assert!(super::super::retain_exec_claim(ctx, 918044, 51));
    assert!(!super::super::retain_exec_claim(ctx, 918044, 52));
    op.close();
    guarded(&op, || panic!("possible execution fixture"));
    assert!(pending(ctx));
    assert!(!drain.is_drained());
    assert!(op.state.lock().unwrap().data.is_some());
    assert!(super::super::retain_exec_claim(ctx, 918044, 51));
}

#[cfg(target_os = "macos")]
#[test]
fn source_input_seal_preserves_later_typing_without_granting_execution() {
    for seal_first in [false, true] {
        let handle = crate::session_store::test_handle_on(918050 + u64::from(seal_first), -1);
        let ctx = &handle.ctx;
        // Only kernel/data/input ownership is injected. No signed target or
        // SDK constructor is present, so this fixture can never produce Go.
        let (server, _client) = aterm_uds::CtlStream::pair().unwrap();
        let _direct = crate::control::DirectPeer::enter();
        let _serving = aterm_control::hangup::ServingFd::enter(server.as_raw_fd());
        let operation = "d".repeat(32);
        let (op, drain) = resource_root(ctx, &operation);
        assert!(op.current(ctx, &Seen::of(ctx)));
        if seal_first {
            let mut resume = |_: &str| panic!("input sealing cannot resume work");
            let reply = super::super::serve(
                ctx,
                -1,
                &format!("seal-input source-exec operation={operation}"),
                &mut resume,
            );
            assert!(reply.contains("input=sealed"));
            assert!(op.status().contains("input=sealed"));
        }
        ctx.human_input
            .note(crate::metrics::now_us(), crate::human_input::Gesture::Typed);
        assert_eq!(op.current(ctx, &Seen::of(ctx)), seal_first);
        assert_eq!(op.seal_input(ctx, &Seen::of(ctx)), seal_first);
        let state = op.state.lock().unwrap();
        assert!(!state.possible && !state.granted && state.snapshot.is_none());
        assert!(matches!(state.phase, Phase::Preparing));
        drop(state);
        op.close();
        assert!(!op.seal_input(ctx, &Seen::of(ctx)));
        assert!(drain.is_drained());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_source_input_seal_does_not_bypass_a_hold_cancellation_or_source_generation() {
    use crate::fabric::{Hold, apply_hold_for_test};
    for cause in ["hold", "cancel", "generation"] {
        let handle = crate::session_store::test_handle_on(918052, -1);
        let ctx = &handle.ctx;
        let (mut op, drain) = resource_root(ctx, &"e".repeat(32));
        assert!(op.seal_input(ctx, &Seen::of(ctx)));
        match cause {
            "hold" => {
                assert!(apply_hold_for_test(
                    ctx,
                    Some(Hold {
                        reason: "fixture-held".into(),
                        origin: "local".into()
                    })
                ));
            }
            "cancel" => {
                assert!(cancel_owner(ctx));
            }
            _ => {
                ctx.fenced_writes.native.lock().unwrap().source_exec.take();
                Arc::get_mut(&mut op).unwrap().generation += 1;
            }
        }
        assert!(!op.seal_input(ctx, &Seen::of(ctx)), "{cause}");
        if cause != "generation" {
            assert!(!op.current(ctx, &Seen::of(ctx)), "{cause}");
        } else {
            assert!(!op.source_current());
        }
        op.close();
        assert!(drain.is_drained());
    }
}

/// The session's Meta is the ranked lock. Beneath it the hold is read from the copy each hold
/// change stores, while a read through the fabric's Inbox is the reversed order, refused before
/// it can block (`crate::fabric::lock_rank`).
#[test]
fn beneath_a_session_meta_guard_the_hold_is_read_without_the_inbox() {
    let handle = crate::session_store::test_handle_on(918062, -1);
    let ctx = &handle.ctx;
    assert!(crate::fabric::apply_hold_for_test(
        ctx,
        Some(crate::fabric::Hold {
            reason: "fixture-held".into(),
            origin: "local".into(),
        })
    ));
    let meta = ctx.meta.lock().unwrap();
    assert_eq!(
        ctx.fabric.hold().map(|hold| hold.reason),
        Some("fixture-held".to_string())
    );
    let reversed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ctx.fabric.hold_observation()
    }));
    assert!(
        reversed.is_err(),
        "Meta, then the Inbox, is the reversed order"
    );
    drop(meta);
    assert!(Seen::of(ctx).held);
}

/// A source-exec verb served on a control connection's thread and a task mark on the same
/// session, from two threads. The mark holds the session's fabric Inbox and has not yet taken
/// Meta when the verb starts; the mark goes on once the verb has answered or waits on that
/// Inbox. A verb that waited on the Inbox beneath Meta to read the hold would hold Meta waiting
/// on the Inbox, while the mark holds the Inbox waiting on Meta: both would wait forever, and
/// every later reader of the session's Meta with them.
#[cfg(target_os = "macos")]
#[test]
fn a_source_exec_verb_and_a_task_mark_on_one_session_never_deadlock() {
    use aterm_spec::wait::MUST_HAPPEN;
    let handle = crate::session_store::test_handle_on(918060, -1);
    let ctx = Arc::clone(&handle.ctx);
    let operation = "f".repeat(32);
    let (op, drain) = resource_root(&ctx, &operation);
    let (holds, go) = ctx.fabric.pause_next_unfenced();
    let mark = {
        let ctx = Arc::clone(&ctx);
        std::thread::spawn(move || {
            crate::task_record::mark_session_task_with_context(
                &ctx,
                918060,
                None,
                crate::task_record::AttentionEdit::Keep,
                MUST_HAPPEN,
                |_, _, _, _| false,
            )
            .is_ok()
        })
    };
    holds
        .recv_timeout(MUST_HAPPEN)
        .expect("the task mark holds the session's Inbox");
    let waits = ctx.fabric.inbox_waits();
    let verb = {
        let ctx = Arc::clone(&ctx);
        let line = format!("seal-input operation={operation}");
        std::thread::spawn(move || {
            let (server, _client) = aterm_uds::CtlStream::pair().unwrap();
            let _direct = crate::control::DirectPeer::enter();
            let _serving = aterm_control::hangup::ServingFd::enter(server.as_raw_fd());
            serve(&ctx, -1, &line)
        })
    };
    aterm_spec::wait::until_within(
        MUST_HAPPEN,
        "the verb answers or waits on the Inbox the task mark holds",
        || (ctx.fabric.inbox_waits() > waits || verb.is_finished()).then_some(()),
    );
    go.send(()).unwrap();
    let both = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(10), || {
        (mark.is_finished() && verb.is_finished()).then_some(())
    });
    assert!(
        both.is_some(),
        "deadlock: the verb holds Meta waiting on the Inbox, the task mark the Inbox waiting on Meta"
    );
    assert!(verb.join().unwrap().contains("input=sealed"));
    assert!(mark.join().unwrap());
    op.close();
    assert!(drain.is_drained());
}

/// Run [`hold_to_end`] as the operation's worker does once [`run`] returns.
#[cfg(target_os = "macos")]
fn worker_holds_to_end(
    ctx: &Arc<crate::SessionCtx>,
    op: &Arc<Operation>,
) -> std::thread::JoinHandle<()> {
    let session = Arc::downgrade(ctx);
    let op = Arc::clone(op);
    std::thread::spawn(move || hold_to_end(&op, &session))
}

/// A possible execution whose constructor was never granted sent no Go and can admit no
/// successor: once its worker is done, its custody, DATA and the claim the EOF kept all end.
#[cfg(target_os = "macos")]
#[test]
fn an_ungranted_possible_execution_ends_its_custody_and_the_claim_kept_for_it() {
    use aterm_spec::wait::MUST_HAPPEN;
    let handle = crate::session_store::test_handle_on(918061, -1);
    let ctx = &handle.ctx;
    crate::session_timeline::claim_supervisor_as(
        ctx,
        &format!("codex-alab@{}", std::process::id()),
        Some(61),
        None,
        Some(crate::self_claim::Proven {
            pid: std::process::id(),
            start: "resource-fixture".into(),
        }),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .unwrap();
    let (op, drain) = resource_root(ctx, &"a".repeat(32));
    op.state.lock().unwrap().possible = true;
    assert!(
        super::super::retain_exec_claim(ctx, 918061, 61),
        "the EOF keeps the claim"
    );
    op.close();
    assert!(
        pending(ctx),
        "closing a possible execution keeps its custody"
    );
    let worker = worker_holds_to_end(ctx, &op);
    let ended = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(10), || {
        worker.is_finished().then_some(())
    });
    assert!(
        ended.is_some(),
        "the worker held an ungranted operation's custody"
    );
    worker.join().unwrap();
    assert!(!pending(ctx));
    assert!(drain.is_drained());
    assert!(op.state.lock().unwrap().data.is_none());
    assert!(op.status().contains("state=closed"));
    assert!(!super::super::retain_exec_claim(ctx, 918061, 61));
    assert!(
        ctx.meta
            .lock()
            .unwrap()
            .live_supervisor(crate::metrics::now_us())
            .is_none(),
        "the claim the EOF kept goes back with the custody"
    );
}

/// A granted possible execution that can start nothing more (cancelled, or past its deadline
/// before activation) stops holding the app handoff at once; its input position and DATA stay
/// while its source process may still run a granted successor, and end once that process is gone,
/// here before its close.
#[cfg(target_os = "macos")]
#[test]
fn a_granted_possible_execution_frees_the_handoff_then_ends_when_its_source_is_gone() {
    use aterm_spec::wait::MUST_HAPPEN;
    for (local, ending) in [(918063, "cancelled"), (918064, "expired")] {
        let handle = crate::session_store::test_handle_on(local, -1);
        let ctx = &handle.ctx;
        let mut child = aterm_uds::spawnfd::command("/bin/cat")
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .unwrap();
        let input = aterm_uds::deadline::cat_echoes(&mut child);
        let source = Process::observe(child.id()).unwrap();
        let now = aterm_agent::harness::native_act::monotonic_us().unwrap();
        let deadline = if ending == "expired" {
            now
        } else {
            now + 60_000_000
        };
        let (op, drain) = resource_root_of(ctx, &"b".repeat(32), source, deadline);
        {
            let mut state = op.state.lock().unwrap();
            state.possible = true;
            state.granted = true;
        }
        if ending == "cancelled" {
            op.close();
        }
        let worker = worker_holds_to_end(ctx, &op);
        aterm_spec::wait::until_within(MUST_HAPPEN, "the app handoff is free", || {
            op.state
                .lock()
                .unwrap()
                .custody
                .as_ref()
                .is_some_and(|custody| custody.handoff_guard().is_none())
                .then_some(())
        });
        assert!(pending(ctx), "{ending}: the input position stays");
        assert!(!drain.is_drained(), "{ending}");
        assert!(op.state.lock().unwrap().data.is_some(), "{ending}");
        assert!(!worker.is_finished(), "{ending}");
        drop(input);
        aterm_spec::wait::until_within(MUST_HAPPEN, "cat exits once its input closes", || {
            child.try_wait().unwrap().map(|_| ())
        });
        let ended = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(10), || {
            worker.is_finished().then_some(())
        });
        assert!(ended.is_some(), "{ending}: the worker outlived its source");
        worker.join().unwrap();
        assert!(!pending(ctx), "{ending}");
        assert!(drain.is_drained(), "{ending}");
        assert!(op.state.lock().unwrap().data.is_none(), "{ending}");
    }
}

/// One pass of the worker over a possible execution: it ends at the close whatever is still to
/// come, and before the close it stops holding the app handoff once nothing more can start,
/// keeping it for an activated successor's settlement, which the close alone bounds.
#[test]
fn a_pass_ends_at_the_close_and_keeps_the_handoff_only_for_a_settlement_to_come() {
    let fresh = Standing {
        granted: true,
        source_gone: false,
        cancelled: false,
        activated: false,
        past_deadline: false,
        past_close: false,
    };
    assert_eq!(fresh.pass(), Pass::Keep, "a Go may still go out");
    let ungranted = Standing {
        granted: false,
        ..fresh
    };
    assert_eq!(ungranted.pass(), Pass::End, "no Go went out");
    let gone = Standing {
        source_gone: true,
        ..fresh
    };
    assert_eq!(gone.pass(), Pass::End, "nothing of it runs on");
    for activated in [false, true] {
        for cancelled in [false, true] {
            let closed = Standing {
                cancelled,
                activated,
                past_deadline: true,
                past_close: true,
                ..fresh
            };
            assert_eq!(
                closed.pass(),
                Pass::End,
                "past the close: activated={activated} cancelled={cancelled}"
            );
        }
        let cancelled = Standing {
            cancelled: true,
            activated,
            ..fresh
        };
        assert_eq!(
            cancelled.pass(),
            Pass::FreeHandoff,
            "cancelled: activated={activated}"
        );
    }
    let expired = Standing {
        past_deadline: true,
        ..fresh
    };
    assert_eq!(
        expired.pass(),
        Pass::FreeHandoff,
        "past its deadline before activation, nothing more starts"
    );
    let settling = Standing {
        activated: true,
        past_deadline: true,
        ..fresh
    };
    assert_eq!(
        settling.pass(),
        Pass::Keep,
        "an activated successor may settle until the close"
    );
}

/// A granted possible execution whose source still runs, past its close: nothing of it is
/// admitted any more, so its worker ends it at once. Its custody, DATA and the claim the EOF kept
/// go, and the window hears that the session's metadata moved.
#[cfg(target_os = "macos")]
#[test]
fn a_granted_possible_execution_ends_at_its_close_while_its_source_runs() {
    let local = 918065;
    let handle = crate::session_store::test_handle_on(local, -1);
    let ctx = &handle.ctx;
    crate::session_timeline::claim_supervisor_as(
        ctx,
        &format!("codex-alab@{}", std::process::id()),
        Some(65),
        None,
        Some(crate::self_claim::Proven {
            pid: std::process::id(),
            start: "resource-fixture".into(),
        }),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .unwrap();
    let now = aterm_agent::harness::native_act::monotonic_us().unwrap();
    let closed = now
        .checked_sub(aterm_agent::harness::native_act::COMPLETION_US + 1)
        .expect("the boot is older than a close");
    let source = Process::observe(std::process::id()).unwrap();
    let (op, drain) = resource_root_of(ctx, &"c".repeat(32), source, closed);
    {
        let mut state = op.state.lock().unwrap();
        state.possible = true;
        state.granted = true;
    }
    assert!(
        super::super::retain_exec_claim(ctx, local, 65),
        "the EOF keeps the claim"
    );
    let worker = {
        let session = Arc::downgrade(ctx);
        let op = Arc::clone(&op);
        std::thread::spawn(move || {
            hold_to_end(&op, &session);
            crate::presence::take_meta_wakes()
        })
    };
    let ended = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(10),
        || worker.is_finished().then_some(()),
    );
    assert!(
        ended.is_some(),
        "the worker held a closed operation's custody"
    );
    assert_eq!(
        worker.join().unwrap(),
        vec![local],
        "the window hears the session's metadata moved"
    );
    assert!(!runtime_data::gone(&op.source), "its source still runs");
    assert!(!pending(ctx));
    assert!(drain.is_drained());
    assert!(op.state.lock().unwrap().data.is_none());
    assert!(op.status().contains("state=closed"));
    assert!(
        op.cancelled.load(std::sync::atomic::Ordering::Acquire),
        "a successor's settlement is refused from here on"
    );
    assert!(
        ctx.meta
            .lock()
            .unwrap()
            .live_supervisor(crate::metrics::now_us())
            .is_none(),
        "the claim the EOF kept goes back with the custody"
    );
}

fn capability_fixture() -> aterm_json::Value {
    aterm_json::json!({
        "protocol": 3, "full_runtime": 3, "runtime_readers": [2, 3],
        "extensions": {"future_optional": {"enabled": true}},
        "exec_custody": 2, "exec_custody_adopted": false,
        "pid": 42, "executable": "/fixture/codex", "device": 1,
        "inode": 2, "product_version": "fixture"
    })
}

#[test]
fn source_exec_capabilities_negotiate_readers_without_writer_equality() {
    let value = capability_fixture();
    let capabilities: Capabilities = aterm_json::from_value(value.clone()).unwrap();
    assert!(capabilities.accepts_runtime(2));
    assert!(capabilities.accepts_runtime(3));
    assert!(!capabilities.accepts_runtime(0));
    assert!(!capabilities.accepts_runtime(1));
    assert!(!capabilities.accepts_runtime(4));
    for (field, replacement) in [
        ("protocol", aterm_json::json!(2)),
        ("full_runtime", aterm_json::json!(0)),
        ("exec_custody", aterm_json::json!(1)),
        ("exec_custody_adopted", aterm_json::json!(true)),
        ("pid", aterm_json::json!(1)),
        ("runtime_readers", aterm_json::json!([])),
        ("runtime_readers", aterm_json::json!([0, 2, 3])),
        ("runtime_readers", aterm_json::json!([2, 2, 3])),
        ("runtime_readers", aterm_json::json!([3, 2])),
        ("runtime_readers", aterm_json::json!([2])),
    ] {
        let mut wrong = value.clone();
        wrong
            .as_object_mut()
            .unwrap()
            .insert(field.into(), replacement);
        let capabilities: Capabilities = aterm_json::from_value(wrong).unwrap();
        assert!(!capabilities.accepts_runtime(2), "{field}");
    }
    let mut missing = value.clone();
    missing.as_object_mut().unwrap().remove("runtime_readers");
    assert!(aterm_json::from_value::<Capabilities>(missing).is_err());
    let mut outside_extensions = value.clone();
    outside_extensions
        .as_object_mut()
        .unwrap()
        .insert("future_optional".into(), aterm_json::json!(true));
    assert!(aterm_json::from_value::<Capabilities>(outside_extensions).is_err());
    let mut no_extensions = value;
    no_extensions.as_object_mut().unwrap().remove("extensions");
    assert!(
        aterm_json::from_value::<Capabilities>(no_extensions)
            .unwrap()
            .accepts_runtime(2)
    );
}

#[test]
#[ignore = "requires an explicitly built Codex CLI; checks compatibility, not native admission"]
fn packaged_codex_capabilities_parse_with_the_native_reader_contract() {
    let mut command = aterm_uds::spawnfd::command(
        std::env::var_os("ALAB_TEST_UPGRADE_TARGET").expect("explicit packaged Codex CLI"),
    );
    command.args(["debug", "upgrade-capabilities"]);
    let output = aterm_uds::deadline::output_until_complete_stdout_with_limit(
        &mut command,
        std::time::Instant::now() + Duration::from_secs(60),
        16 * 1024,
    )
    .unwrap();
    assert!(output.status.success());
    let capabilities: Capabilities = aterm_json::from_slice(&output.stdout).unwrap();
    assert!(capabilities.accepts_runtime(capabilities.full_runtime));
}

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
        ("cancel-prepare source-exec", false),
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
        "prepare source-exec operation=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa build=1 primary=00000000-0000-4000-8000-000000000001",
        "info source-exec prepare operation=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "execute source-exec operation=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
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
    let request = "prepare operation=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa build=1 primary=00000000-0000-4000-8000-000000000001";
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
    let canonical = "prepare operation=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa build=1 primary=00000000-0000-4000-8000-000000000001";
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
    let source = Process::observe(std::process::id()).unwrap();
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
            primary: "00000000-0000-4000-8000-000000000001".into(),
        },
        generation: generation(source.pid).unwrap(),
        source,
        authority,
        human_seq: ctx.human_input.seq(),
        deadline: aterm_agent::harness::native_act::monotonic_us().unwrap() + 60_000_000,
        cancelled: std::sync::atomic::AtomicBool::new(false),
        state: Mutex::new(State {
            phase: Phase::Preparing,
            custody: Some(Arc::new(custody)),
            data: Some(pin),
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
            let meta = ctx.meta.lock().unwrap();
            assert!(cancel_owner(ctx));
            assert!(pending(ctx), "a metadata-side stop only requests closing");
            assert!(!op.current(ctx, &meta.supervisor_authority(crate::metrics::now_us())));
            drop(meta);
            guarded(&op, || op.close());
        }
        assert!(!pending(ctx));
        assert!(drain.is_drained());
        assert!(op.status().contains("state=closed"));
    }
}

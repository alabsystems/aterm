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
            source_build: 1,
            primary: "00000000-0000-4000-8000-000000000001".into(),
        },
        generation: generation(source.pid).unwrap(),
        source,
        authority,
        human_seq: ctx.human_input.seq(),
        input_sealed: std::sync::atomic::AtomicBool::new(false),
        deadline: aterm_agent::harness::native_act::monotonic_us().unwrap() + 60_000_000,
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
    assert!(!super::super::retain_exec_claim(ctx, 51));
    op.state.lock().unwrap().possible = true;
    assert!(super::super::retain_exec_claim(ctx, 51));
    assert!(!super::super::retain_exec_claim(ctx, 52));
    op.close();
    guarded(&op, || panic!("possible execution fixture"));
    assert!(pending(ctx));
    assert!(!drain.is_drained());
    assert!(op.state.lock().unwrap().data.is_some());
    assert!(super::super::retain_exec_claim(ctx, 51));
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
        let authority = ctx
            .meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us());
        assert!(op.current(ctx, &authority));
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
        assert_eq!(op.current(ctx, &authority), seal_first);
        assert_eq!(op.seal_input(ctx, &authority), seal_first);
        let state = op.state.lock().unwrap();
        assert!(!state.possible && !state.granted && state.snapshot.is_none());
        assert!(matches!(state.phase, Phase::Preparing));
        drop(state);
        op.close();
        assert!(!op.seal_input(ctx, &authority));
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
        let authority = ctx
            .meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us());
        assert!(op.seal_input(ctx, &authority));
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
        assert!(!op.seal_input(ctx, &authority), "{cause}");
        if cause != "generation" {
            assert!(!op.current(ctx, &authority), "{cause}");
        } else {
            assert!(!op.source_current());
        }
        op.close();
        assert!(drain.is_drained());
    }
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

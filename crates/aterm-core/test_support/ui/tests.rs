// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for UI Bridge.

use super::*;

#[test]
fn test_ui_bridge_new() {
    let bridge = UIBridge::new();
    assert_eq!(bridge.state(), UIState::Idle);
    assert_eq!(bridge.pending_count(), 0);
    assert!(bridge.is_consistent());
}

#[test]
fn test_terminal_lifecycle() {
    let mut bridge = UIBridge::new();
    let t0 = TerminalId::from_raw(0);

    // Initially inactive
    assert_eq!(bridge.terminal_state(t0), TerminalState::Inactive);

    // Create terminal
    bridge.handle_event(Event::create_terminal(t0)).unwrap();
    assert_eq!(bridge.terminal_state(t0), TerminalState::Active);
    assert!(bridge.is_consistent());

    // Destroy terminal
    bridge.handle_event(Event::destroy_terminal(t0)).unwrap();
    assert_eq!(bridge.terminal_state(t0), TerminalState::Disposed);
    assert!(bridge.is_consistent());
}

#[test]
fn test_disposed_terminal_cannot_be_reactivated() {
    let mut bridge = UIBridge::new();
    let t0 = TerminalId::from_raw(0);

    // Create and destroy
    bridge.handle_event(Event::create_terminal(t0)).unwrap();
    bridge.handle_event(Event::destroy_terminal(t0)).unwrap();

    // Cannot create again (disposed is permanent)
    let result = bridge.handle_event(Event::create_terminal(t0));
    assert_eq!(result, Err(UIError::InvalidTerminalState));
}

#[test]
fn test_input_requires_active_terminal() {
    let mut bridge = UIBridge::new();
    let t0 = TerminalId::from_raw(0);

    // Input to inactive terminal should fail
    let result = bridge.handle_event(Event::input(t0));
    assert_eq!(result, Err(UIError::InvalidTerminalState));

    // Create terminal first
    bridge.handle_event(Event::create_terminal(t0)).unwrap();

    // Now input should work
    bridge.handle_event(Event::input(t0)).unwrap();
    assert!(bridge.is_consistent());
}

#[test]
fn test_render_flow() {
    let mut bridge = UIBridge::new();
    let t0 = TerminalId::from_raw(0);

    // Create terminal
    bridge.handle_event(Event::create_terminal(t0)).unwrap();
    assert_eq!(bridge.state(), UIState::Idle);

    // Request render
    bridge.handle_event(Event::render(t0)).unwrap();
    assert_eq!(bridge.state(), UIState::Rendering);
    assert_eq!(bridge.render_pending_count(), 1);
    assert!(bridge.is_consistent());

    // Complete render
    bridge.complete_render(t0).unwrap();
    assert_eq!(bridge.state(), UIState::Idle);
    assert_eq!(bridge.render_pending_count(), 0);
    assert!(bridge.is_consistent());
}

#[test]
fn test_callback_flow() {
    let mut bridge = UIBridge::new();
    let t0 = TerminalId::from_raw(0);
    let cb42 = CallbackId::from_raw(42);

    // Create terminal
    bridge.handle_event(Event::create_terminal(t0)).unwrap();

    // Request callback
    bridge
        .handle_event(Event::request_callback(t0, cb42))
        .unwrap();
    assert_eq!(bridge.state(), UIState::WaitingForCallback);
    assert_eq!(bridge.callback_count(), 1);
    assert!(bridge.is_consistent());

    // Complete callback
    bridge.complete_callback(cb42).unwrap();
    assert_eq!(bridge.state(), UIState::Idle);
    assert_eq!(bridge.callback_count(), 0);
    assert!(bridge.is_consistent());
}

#[test]
fn test_shutdown() {
    let mut bridge = UIBridge::new();
    let t0 = TerminalId::from_raw(0);

    // Enqueue some events
    bridge.handle_event(Event::create_terminal(t0)).unwrap();
    bridge.enqueue(Event::input(t0)).unwrap();
    bridge.enqueue(Event::input(t0)).unwrap();

    // Shutdown
    bridge.enqueue(Event::shutdown()).unwrap();

    // Process until shutdown (with safety limit)
    let mut rounds = 0;
    let max_rounds = 100;
    while bridge.state() != UIState::ShuttingDown {
        rounds += 1;
        assert!(
            rounds <= max_rounds,
            "UIBridge did not reach ShuttingDown after {max_rounds} rounds"
        );
        bridge.start_processing().unwrap();
        bridge.complete_processing().unwrap();
    }

    assert_eq!(bridge.state(), UIState::ShuttingDown);
    assert_eq!(bridge.pending_count(), 0);
    assert!(bridge.is_consistent());

    // Cannot enqueue after shutdown
    let result = bridge.enqueue(Event::input(t0));
    assert_eq!(result, Err(UIError::ShuttingDown));
}

#[test]
fn test_queue_full() {
    let mut bridge = UIBridge::new();
    let t0 = TerminalId::from_raw(0);
    bridge.handle_event(Event::create_terminal(t0)).unwrap();

    // Fill the queue
    for _ in 0..MAX_QUEUE {
        bridge.enqueue(Event::input(t0)).unwrap();
    }

    // Next enqueue should fail
    let result = bridge.enqueue(Event::input(t0));
    assert_eq!(result, Err(UIError::QueueFull));
}

#[test]
fn test_no_duplicate_callbacks() {
    let mut bridge = UIBridge::new();
    let t0 = TerminalId::from_raw(0);
    let cb42 = CallbackId::from_raw(42);
    bridge.handle_event(Event::create_terminal(t0)).unwrap();

    // Request callback
    bridge
        .handle_event(Event::request_callback(t0, cb42))
        .unwrap();

    // Duplicate callback should fail (even if enqueued)
    let result = bridge.enqueue(Event::request_callback(t0, cb42));
    assert_eq!(result, Err(UIError::DuplicateCallback));
}

#[test]
fn test_terminal_id_bounds() {
    let mut bridge = UIBridge::new();

    // Valid terminal ID (at MAX_TERMINALS - 1) should succeed
    let valid_id = TerminalId::from_raw(u32::try_from(MAX_TERMINALS - 1).unwrap());
    assert!(
        bridge
            .handle_event(Event::create_terminal(valid_id))
            .is_ok()
    );

    // Invalid terminal ID (at MAX_TERMINALS) should fail
    let invalid_id = TerminalId::from_raw(u32::try_from(MAX_TERMINALS).unwrap());
    let result = bridge.handle_event(Event::create_terminal(invalid_id));
    assert_eq!(result, Err(UIError::InvalidTerminalId));

    // Way out of bounds should also fail
    let result = bridge.handle_event(Event::create_terminal(TerminalId::from_raw(u32::MAX)));
    assert_eq!(result, Err(UIError::InvalidTerminalId));
}

#[test]
fn test_callback_id_bounds() {
    let mut bridge = UIBridge::new();
    let t0 = TerminalId::from_raw(0);
    bridge.handle_event(Event::create_terminal(t0)).unwrap();

    // Valid callback ID (at MAX_CALLBACKS - 1) should succeed
    let valid_cb = CallbackId::from_raw(u32::try_from(MAX_CALLBACKS - 1).unwrap());
    assert!(
        bridge
            .handle_event(Event::request_callback(t0, valid_cb))
            .is_ok()
    );

    // Complete callback to return to Idle
    bridge.complete_callback(valid_cb).unwrap();

    // Invalid callback ID (at MAX_CALLBACKS) should fail
    let invalid_cb = CallbackId::from_raw(u32::try_from(MAX_CALLBACKS).unwrap());
    let result = bridge.handle_event(Event::request_callback(t0, invalid_cb));
    assert_eq!(result, Err(UIError::InvalidTerminalId));
}

#[test]
fn test_events_preserved_invariant() {
    let mut bridge = UIBridge::new();
    let t0 = TerminalId::from_raw(0);
    bridge.handle_event(Event::create_terminal(t0)).unwrap();

    // Enqueue several events
    for _ in 0_u8..10 {
        bridge.enqueue(Event::input(t0)).unwrap();
    }

    // Invariant should hold
    assert!(bridge.is_consistent());

    // Process some events
    for _ in 0..5 {
        bridge.start_processing().unwrap();
        bridge.complete_processing().unwrap();
    }

    // Invariant should still hold
    assert!(bridge.is_consistent());
}

#[test]
fn test_state_machine_transitions() {
    let mut bridge = UIBridge::new();
    let t0 = TerminalId::from_raw(0);

    // Idle -> Processing
    bridge.handle_event(Event::create_terminal(t0)).unwrap();
    bridge.enqueue(Event::render(t0)).unwrap();
    bridge.start_processing().unwrap();
    assert_eq!(bridge.state(), UIState::Processing);

    // Processing -> Rendering
    bridge.complete_processing().unwrap();
    assert_eq!(bridge.state(), UIState::Rendering);

    // Rendering -> Idle
    bridge.complete_render(t0).unwrap();
    assert_eq!(bridge.state(), UIState::Idle);
}

/// Test exact fuzzer sequence that found DisposedMonotonic bug.
/// Traces through the exact minimized sequence from the fuzzer.
#[test]
fn test_fuzzer_sequence_disposed_monotonic() {
    let mut bridge = UIBridge::new();
    let mut observed_disposed = std::collections::HashSet::new();
    let t0 = TerminalId::from_raw(0);
    let t3 = TerminalId::from_raw(3);
    let t7 = TerminalId::from_raw(7);

    // Helper to check and record disposed terminals
    fn check_observed(bridge: &UIBridge, observed: &mut std::collections::HashSet<TerminalId>) {
        for raw in 0..=8u32 {
            let tid = TerminalId::from_raw(raw);
            if bridge.terminal_state(tid) == TerminalState::Disposed {
                observed.insert(tid);
            }
        }
    }

    // Helper to verify monotonicity
    fn verify_monotonicity(
        bridge: &UIBridge,
        observed: &std::collections::HashSet<TerminalId>,
        step: &str,
    ) {
        for tid in observed {
            assert_eq!(
                bridge.terminal_state(*tid),
                TerminalState::Disposed,
                "DisposedMonotonic violated at step '{}': terminal {} changed from Disposed to {:?}",
                step,
                tid,
                bridge.terminal_state(*tid)
            );
        }
    }

    // Action 0: Render(7) - fails (terminal 7 not active)
    check_observed(&bridge, &mut observed_disposed);
    assert!(
        bridge.handle_event(Event::render(t7)).is_err(),
        "Render(7) should fail: no such terminal"
    );
    verify_monotonicity(&bridge, &observed_disposed, "Render(7)");

    // Action 1: CreateTerminal(3)
    check_observed(&bridge, &mut observed_disposed);
    bridge
        .handle_event(Event::create_terminal(t3))
        .expect("CreateTerminal(3) should succeed");
    verify_monotonicity(&bridge, &observed_disposed, "CreateTerminal(3)");

    // Action 2: Render(3) - puts us in Rendering state
    check_observed(&bridge, &mut observed_disposed);
    bridge
        .handle_event(Event::render(t3))
        .expect("Render(3) should succeed");
    verify_monotonicity(&bridge, &observed_disposed, "Render(3)");

    // Action 3: CreateTerminal(0) - enqueued
    check_observed(&bridge, &mut observed_disposed);
    bridge
        .handle_event(Event::create_terminal(t0))
        .expect("CreateTerminal(0) should enqueue");
    verify_monotonicity(&bridge, &observed_disposed, "CreateTerminal(0) enqueued");

    // Action 4: CreateTerminal(0) - duplicate (should fail due to fix)
    check_observed(&bridge, &mut observed_disposed);
    let result4 = bridge.handle_event(Event::create_terminal(t0));
    assert!(
        result4.is_err(),
        "Duplicate CreateTerminal should be rejected"
    );
    verify_monotonicity(&bridge, &observed_disposed, "CreateTerminal(0) duplicate");

    // Action 5: Shutdown - enqueued
    check_observed(&bridge, &mut observed_disposed);
    bridge
        .handle_event(Event::shutdown())
        .expect("Shutdown should enqueue");
    verify_monotonicity(&bridge, &observed_disposed, "Shutdown enqueued");

    // Action 6: CompleteRender(3)
    check_observed(&bridge, &mut observed_disposed);
    bridge
        .complete_render(t3)
        .expect("CompleteRender(3) should succeed");
    verify_monotonicity(&bridge, &observed_disposed, "CompleteRender(3)");

    // Action 7: StartProcessing
    check_observed(&bridge, &mut observed_disposed);
    bridge
        .start_processing()
        .expect("StartProcessing should succeed");
    verify_monotonicity(&bridge, &observed_disposed, "StartProcessing");

    // Action 8: CompleteProcessing - terminal 0 becomes Active
    check_observed(&bridge, &mut observed_disposed);
    bridge
        .complete_processing()
        .expect("CompleteProcessing should succeed");
    verify_monotonicity(
        &bridge,
        &observed_disposed,
        "CompleteProcessing (CreateTerminal)",
    );

    // Action 9: DestroyTerminal(0) - should execute immediately (we're Idle)
    check_observed(&bridge, &mut observed_disposed);
    bridge
        .handle_event(Event::destroy_terminal(t0))
        .expect("DestroyTerminal(0) should succeed");
    verify_monotonicity(&bridge, &observed_disposed, "DestroyTerminal(0)");

    // At this point terminal 0 should be Disposed
    assert_eq!(
        bridge.terminal_state(t0),
        TerminalState::Disposed,
        "Terminal 0 should be Disposed after DestroyTerminal"
    );

    // Action 10: StartProcessing - gets Shutdown from queue
    check_observed(&bridge, &mut observed_disposed);
    bridge
        .start_processing()
        .expect("StartProcessing (Shutdown) should succeed");
    verify_monotonicity(&bridge, &observed_disposed, "StartProcessing (Shutdown)");

    // Action 11: CreateTerminal(0) - should fail (terminal 0 is Disposed)
    check_observed(&bridge, &mut observed_disposed);
    let result = bridge.handle_event(Event::create_terminal(t0));
    assert!(
        result.is_err(),
        "CreateTerminal(0) should fail because terminal 0 is Disposed"
    );
    verify_monotonicity(
        &bridge,
        &observed_disposed,
        "CreateTerminal(0) after destroy",
    );

    // Action 12: CompleteProcessing - processes Shutdown
    check_observed(&bridge, &mut observed_disposed);
    bridge
        .complete_processing()
        .expect("CompleteProcessing (Shutdown) should succeed");
    verify_monotonicity(&bridge, &observed_disposed, "CompleteProcessing (Shutdown)");

    // Final check
    assert_eq!(
        bridge.terminal_state(t0),
        TerminalState::Disposed,
        "Terminal 0 should still be Disposed after shutdown"
    );
}

// =============================================================================
// SHUTDOWN REGRESSION TESTS
// =============================================================================

/// This test verifies the FIX for the DisposedMonotonic bug.
///
/// The bug (now fixed): When Shutdown was processed, it marked all pending events
/// as "processed" and cleared the queue, but did NOT actually execute DestroyTerminal
/// events. Terminals remained Active, violating DisposedMonotonic.
///
/// The fix: Shutdown now explicitly disposes all active terminals before clearing
/// the queue, ensuring no terminal remains Active after shutdown.
#[test]
fn test_shutdown_disposes_all_active_terminals() {
    let mut bridge = UIBridge::new();
    let t0 = TerminalId::from_raw(0);

    // Create terminal 0
    bridge.handle_event(Event::create_terminal(t0)).unwrap();
    assert_eq!(bridge.terminal_state(t0), TerminalState::Active);
    assert_eq!(bridge.state(), UIState::Idle);

    // Put us in a non-Idle state by requesting a render
    bridge.handle_event(Event::render(t0)).unwrap();
    assert_eq!(bridge.state(), UIState::Rendering);

    // Now enqueue Shutdown FIRST (while in Rendering state)
    bridge.enqueue(Event::shutdown()).unwrap();

    // Then enqueue DestroyTerminal (also while in Rendering state)
    bridge.enqueue(Event::destroy_terminal(t0)).unwrap();

    // Queue is now: [Shutdown, DestroyTerminal(0)]
    assert_eq!(bridge.pending_count(), 2);

    // Complete the render to return to Idle
    bridge.complete_render(t0).unwrap();
    assert_eq!(bridge.state(), UIState::Idle);

    // Now process the queue
    // This will process Shutdown first, which clears the queue
    bridge.start_processing().unwrap();
    bridge.complete_processing().unwrap();

    // We're now ShuttingDown
    assert_eq!(bridge.state(), UIState::ShuttingDown);

    // Queue should be empty (Shutdown cleared it)
    assert_eq!(bridge.pending_count(), 0);

    // Shutdown disposes all active terminals.
    assert_eq!(
        bridge.terminal_state(t0),
        TerminalState::Disposed,
        "Terminal 0 should be Disposed after shutdown"
    );
}

/// Test that handle_event(Shutdown) correctly clears pending events.
///
/// Bug found during iteration 296: handle_event(Shutdown) was only incrementing
/// processed_count by 1, not accounting for pending events. This broke the
/// EventsPreserved invariant when there were pending events.
#[test]
fn test_handle_event_shutdown_clears_pending() {
    let mut bridge = UIBridge::new();
    let t0 = TerminalId::from_raw(0);

    // Create terminal
    bridge.handle_event(Event::create_terminal(t0)).unwrap();
    assert!(bridge.is_consistent());

    // Put bridge in Rendering state
    bridge.handle_event(Event::render(t0)).unwrap();
    assert_eq!(bridge.state(), UIState::Rendering);

    // Enqueue some events while rendering
    bridge.enqueue(Event::input(t0)).unwrap();
    bridge.enqueue(Event::resize(t0, 24, 80)).unwrap();
    assert_eq!(bridge.pending_count(), 2);
    assert!(bridge.is_consistent());

    // Complete render to return to Idle
    bridge.complete_render(t0).unwrap();
    assert_eq!(bridge.state(), UIState::Idle);
    assert_eq!(bridge.pending_count(), 2); // Events still pending

    // Now call handle_event(Shutdown) directly (not via enqueue+process)
    // This tests the direct path which had the bug
    bridge.handle_event(Event::shutdown()).unwrap();

    // Verify state
    assert_eq!(bridge.state(), UIState::ShuttingDown);
    assert_eq!(bridge.pending_count(), 0); // Pending events should be cleared
    assert_eq!(bridge.terminal_state(t0), TerminalState::Disposed);

    // CRITICAL: EventsPreserved invariant must hold
    assert!(
        bridge.is_consistent(),
        "EventsPreserved invariant broken after handle_event(Shutdown)"
    );
}

// =============================================================================
// PROOF-DEMOTED UNIT TESTS (#2594)
//
// These tests replace non-symbolic proofs that ran fixed-path checks.
// The behaviors were either already covered by existing tests above, or
// are newly added here to preserve coverage after proof demotion.
// =============================================================================

#[test]
fn test_event_id_monotonic() {
    let id1 = next_event_id();
    let id2 = next_event_id();
    assert!(id2.0 > id1.0, "Event IDs must be monotonically increasing");
}

#[test]
fn test_processing_state_has_current_event() {
    let mut bridge = UIBridge::new();
    let t0 = TerminalId::from_raw(0);

    bridge.handle_event(Event::create_terminal(t0)).unwrap();
    bridge.enqueue(Event::input(t0)).unwrap();

    bridge.start_processing().unwrap();
    assert_eq!(bridge.state(), UIState::Processing);
    assert!(
        bridge.current_event.is_some(),
        "Processing state must have current_event"
    );
}

#[test]
fn test_shutdown_paths_equivalent() {
    // Path 1: handle_event(Shutdown) directly
    let mut bridge1 = UIBridge::new();
    let t0 = TerminalId::from_raw(0);
    bridge1.handle_event(Event::create_terminal(t0)).unwrap();
    bridge1.handle_event(Event::shutdown()).unwrap();

    // Path 2: enqueue + start_processing + complete_processing
    let mut bridge2 = UIBridge::new();
    bridge2.handle_event(Event::create_terminal(t0)).unwrap();
    bridge2.enqueue(Event::shutdown()).unwrap();
    bridge2.start_processing().unwrap();
    bridge2.complete_processing().unwrap();

    // Both must be in ShuttingDown state
    assert_eq!(bridge1.state(), UIState::ShuttingDown);
    assert_eq!(bridge2.state(), UIState::ShuttingDown);

    // Terminal states must match
    assert_eq!(
        bridge1.terminal_state(t0),
        bridge2.terminal_state(t0),
        "Terminal states differ between direct and enqueued shutdown"
    );

    // Both must be consistent
    assert!(bridge1.is_consistent());
    assert!(bridge2.is_consistent());
}

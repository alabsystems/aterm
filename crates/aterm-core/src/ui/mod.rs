// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! UI Bridge for platform integration.
//!
//! This module provides a bridge between platform UI (macOS/iOS/Windows/Linux)
//! and aterm-core. State transitions are specified and model-checked in TLA+
//! (`tla/UIStateMachine.tla`); the Rust implementation is intended to refine
//! that spec but is not itself mechanically verified against it.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    PLATFORM UI (Native)                          │
//! │  macOS/SwiftUI • iOS/SwiftUI • Windows/WinUI • Linux/GTK        │
//! └─────────────────────────────────────────────────────────────────┘
//!                               │
//!                               │ C FFI
//!                               ▼
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    UI BRIDGE (this module)                       │
//! │                                                                  │
//! │  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐       │
//! │  │  UIState     │───▶│  EventQueue  │───▶│  Callbacks   │       │
//! │  │  (TLA+)      │    │  (Tested)    │    │  (Verified)  │       │
//! │  └──────────────┘    └──────────────┘    └──────────────┘       │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Safety Properties (from TLA+ spec)
//!
//! - **EventsPreserved**: No event is lost between enqueue and process
//! - **NoDuplicateEventIds**: Event IDs are unique within the system
//! - **DisposedMonotonic**: Once a terminal is disposed, it stays disposed
//! - **TypeInvariant**: State machine is always in a valid configuration

mod id_types;
mod processing;
mod types;

pub use id_types::{CallbackId, EventId, TerminalId};
pub(crate) use types::UIResult;
pub use types::{Event, EventKind, TerminalState, UIError, UIState};

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use std::collections::{HashMap, HashSet};

// Type aliases for terminal/callback/event tracking.
type TerminalMap = HashMap<TerminalId, TerminalState>;
type CallbackSet = HashSet<CallbackId>;
type RenderSet = HashSet<TerminalId>;
type EventIdSet = HashSet<EventId>;

/// Maximum number of terminals the UI bridge can track.
pub(crate) const MAX_TERMINALS: usize = 256;

/// Maximum number of events in the queue.
pub const MAX_QUEUE: usize = 1024;

/// Maximum number of pending callbacks.
pub const MAX_CALLBACKS: usize = 64;

/// Global event ID counter for uniqueness.
static NEXT_EVENT_ID: AtomicU64 = AtomicU64::new(0);

/// Generate a unique event ID.
fn next_event_id() -> EventId {
    EventId(NEXT_EVENT_ID.fetch_add(1, Ordering::Relaxed))
}

/// UI Bridge - the verified interface between platform UI and aterm-core.
///
/// This struct implements the state machine defined in `tla/UIStateMachine.tla`.
/// All state transitions are designed to preserve the TLA+ invariants.
#[allow(
    clippy::struct_field_names,
    reason = "state/terminal_states naming is domain-natural"
)]
#[derive(Debug)]
pub struct UIBridge {
    /// Current UI state.
    state: UIState,
    /// Terminal states by ID.
    /// Uses the TerminalMap alias (HashMap).
    terminal_states: TerminalMap,
    /// Event queue (FIFO).
    pending_events: VecDeque<Event>,
    /// Event currently being processed.
    current_event: Option<Event>,
    /// Set of pending callback IDs.
    /// Uses the CallbackSet alias (HashSet).
    callbacks_pending: CallbackSet,
    /// Set of terminals awaiting render completion.
    /// Uses the RenderSet alias (HashSet).
    render_pending: RenderSet,
    /// Count of events received (O(1) memory, replaces unbounded HashSet).
    /// TLA+ EventsPreserved: received_count == processed_count + pending + current
    received_count: u64,
    /// Count of events processed (O(1) memory, replaces unbounded HashSet).
    processed_count: u64,
}

impl Default for UIBridge {
    fn default() -> Self {
        Self::new()
    }
}

// NOTE(#2368): UIBridge methods below must stay `pub` — they are used by the
// fuzz target `fuzz/fuzz_targets/ui_bridge.rs` (external crate boundary).
impl UIBridge {
    /// Create a new UI Bridge in the Idle state.
    pub fn new() -> Self {
        Self {
            state: UIState::Idle,
            terminal_states: TerminalMap::new(),
            pending_events: VecDeque::new(),
            current_event: None,
            callbacks_pending: CallbackSet::new(),
            render_pending: RenderSet::new(),
            received_count: 0,
            processed_count: 0,
        }
    }

    /// Get the current UI state.
    pub fn state(&self) -> UIState {
        self.state
    }

    /// Get the number of pending events.
    pub fn pending_count(&self) -> usize {
        self.pending_events.len()
    }

    /// Get the number of pending callbacks.
    pub fn callback_count(&self) -> usize {
        self.callbacks_pending.len()
    }

    /// Get the number of pending renders.
    pub fn render_pending_count(&self) -> usize {
        self.render_pending.len()
    }

    /// Check if the bridge is consistent (invariant check).
    ///
    /// This method verifies that the TLA+ TypeInvariant holds.
    pub fn is_consistent(&self) -> bool {
        // TypeInvariant from TLA+ spec
        let state_consistent = match self.state {
            UIState::Idle => {
                self.current_event.is_none()
                    && self.callbacks_pending.is_empty()
                    && self.render_pending.is_empty()
            }
            UIState::Processing => self.current_event.is_some(),
            UIState::Rendering => !self.render_pending.is_empty(),
            UIState::WaitingForCallback => !self.callbacks_pending.is_empty(),
            UIState::ShuttingDown => self.current_event.is_none(),
        };

        // Queue bounded
        let queue_bounded = self.pending_events.len() <= MAX_QUEUE;

        // Events preserved: received_count == processed_count + pending + current
        // Using O(1) counters instead of O(n) HashSets - fixes unbounded memory growth
        let pending_count = self.pending_events.len() as u64;
        let current_count = u64::from(self.current_event.is_some());
        let events_preserved =
            self.received_count == self.processed_count + pending_count + current_count;

        // No duplicate pending IDs (event IDs are unique from atomic counter)
        let pending_ids: EventIdSet = self.pending_events.iter().map(|e| e.id).collect();
        let no_duplicates = pending_ids.len() == self.pending_events.len();

        // Callbacks bounded
        let callbacks_bounded = self.callbacks_pending.len() <= MAX_CALLBACKS;

        // Terminal states bounded (enforced by validate_event rejecting ID >= MAX_TERMINALS)
        let terminals_bounded = self.terminal_states.len() <= MAX_TERMINALS;

        state_consistent
            && queue_bounded
            && events_preserved
            && no_duplicates
            && callbacks_bounded
            && terminals_bounded
    }

    /// Get the state of a terminal.
    pub fn terminal_state(&self, id: TerminalId) -> TerminalState {
        self.terminal_states
            .get(&id)
            .copied()
            .unwrap_or(TerminalState::Inactive)
    }

    /// Enqueue an event for processing.
    ///
    /// # TLA+ Correspondence
    /// This implements the `EnqueueEvent` action from the TLA+ spec.
    pub fn enqueue(&mut self, event: Event) -> UIResult<()> {
        // Cannot enqueue if shutting down
        if self.state == UIState::ShuttingDown {
            return Err(UIError::ShuttingDown);
        }

        // Cannot exceed queue capacity
        if self.pending_events.len() >= MAX_QUEUE {
            return Err(UIError::QueueFull);
        }

        // Validate event
        self.validate_event(&event)?;

        // Track event count (O(1) counter replaces O(n) HashSet)
        self.received_count += 1;

        // Add to queue
        self.pending_events.push_back(event);

        Ok(())
    }
}

#[cfg(test)]
#[path = "../../test_support/ui/tests.rs"]
mod tests;

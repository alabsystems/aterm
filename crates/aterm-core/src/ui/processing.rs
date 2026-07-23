// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! UIBridge event validation and lifecycle transitions.

use super::{
    CallbackId, Event, EventKind, MAX_CALLBACKS, MAX_TERMINALS, TerminalId, TerminalState,
    UIBridge, UIError, UIResult, UIState,
};

impl UIBridge {
    /// Validate an event before enqueueing.
    ///
    /// # Bounds Enforcement
    /// This method enforces MAX_TERMINALS and MAX_CALLBACKS bounds.
    /// These bounds are enforced here and exercised by tests.
    pub(super) fn validate_event(&self, event: &Event) -> UIResult<()> {
        // BOUNDS CHECK: Terminal ID must be < MAX_TERMINALS
        // Without this, terminal_states HashMap could grow unbounded.
        if let Some(id) = event.terminal {
            if id.raw() as usize >= MAX_TERMINALS {
                return Err(UIError::InvalidTerminalId);
            }
        }

        // BOUNDS CHECK: Callback ID must be < MAX_CALLBACKS
        // Without this, callbacks_pending could reference unbounded IDs.
        if let Some(cb) = event.callback {
            if cb.raw() as usize >= MAX_CALLBACKS {
                return Err(UIError::InvalidTerminalId); // Reusing error type
            }
        }

        match event.kind {
            EventKind::Shutdown => {
                // Shutdown event has no terminal
                if event.terminal.is_some() {
                    return Err(UIError::InvalidTerminalId);
                }
            }
            EventKind::CreateTerminal => {
                // Must target an inactive terminal
                if let Some(id) = event.terminal {
                    if self.terminal_state(id) != TerminalState::Inactive {
                        return Err(UIError::InvalidTerminalState);
                    }
                    // Also check that no CreateTerminal is already pending for this ID
                    // This prevents multiple CreateTerminal events being queued for the same
                    // terminal, which could lead to DisposedMonotonic violations if the terminal
                    // is destroyed between processing them.
                    let has_pending_create = self
                        .pending_events
                        .iter()
                        .any(|e| e.kind == EventKind::CreateTerminal && e.terminal == Some(id));
                    if has_pending_create {
                        return Err(UIError::InvalidTerminalState);
                    }
                } else {
                    return Err(UIError::InvalidTerminalId);
                }
            }
            EventKind::DestroyTerminal
            | EventKind::Input
            | EventKind::Resize
            | EventKind::Render => {
                // Must target an active terminal
                if let Some(id) = event.terminal {
                    if self.terminal_state(id) != TerminalState::Active {
                        return Err(UIError::InvalidTerminalState);
                    }
                } else {
                    return Err(UIError::InvalidTerminalId);
                }
            }
            EventKind::RequestCallback => {
                // Must target an active terminal and have unique callback ID
                if let Some(id) = event.terminal {
                    if self.terminal_state(id) != TerminalState::Active {
                        return Err(UIError::InvalidTerminalState);
                    }
                } else {
                    return Err(UIError::InvalidTerminalId);
                }
                if let Some(cb) = event.callback {
                    if self.callbacks_pending.contains(&cb) {
                        return Err(UIError::DuplicateCallback);
                    }
                }
            }
        }
        Ok(())
    }

    /// Start processing the next event.
    ///
    /// # TLA+ Correspondence
    /// This implements the `StartProcessing` action from the TLA+ spec.
    pub fn start_processing(&mut self) -> UIResult<&Event> {
        // Can only start processing from Idle state
        if self.state != UIState::Idle {
            return Err(UIError::InvalidStateTransition);
        }

        // Must have events to process
        if self.pending_events.is_empty() {
            return Err(UIError::NoEventPending);
        }

        // Must not have a current event
        if self.current_event.is_some() {
            return Err(UIError::InvalidStateTransition);
        }

        // Dequeue and transition — is_empty() check above guarantees Some.
        let Some(event) = self.pending_events.pop_front() else {
            return Err(UIError::NoEventPending);
        };
        self.current_event = Some(event);
        self.state = UIState::Processing;

        // current_event was just set to Some above.
        let Some(ref current) = self.current_event else {
            return Err(UIError::NoEventPending);
        };
        Ok(current)
    }

    /// Execute shutdown logic - SINGLE SOURCE OF TRUTH.
    ///
    /// This method is called by both `complete_processing(Shutdown)` and
    /// `handle_event(Shutdown)` to ensure identical behavior. Having a single
    /// implementation makes divergence bugs structurally impossible.
    ///
    /// # Formal Verification
    /// This consolidation was added after finding a bug where `handle_event(Shutdown)`
    /// had different logic than `complete_processing(Shutdown)`. By extracting to a
    /// single method, tests need only cover this one implementation.
    fn execute_shutdown(&mut self) {
        // 1. Dispose ALL active terminals (DisposedMonotonic invariant)
        let active_terminals: Vec<TerminalId> = self
            .terminal_states
            .iter()
            .filter(|(_, state)| **state == TerminalState::Active)
            .map(|(id, _)| *id)
            .collect();
        for id in active_terminals {
            self.terminal_states.insert(id, TerminalState::Disposed);
        }

        // 2. Mark all pending events as processed (EventsPreserved invariant)
        //    pending_events.len() events + 1 for shutdown event itself
        self.processed_count += self.pending_events.len() as u64 + 1;
        self.pending_events.clear();

        // 3. Clear all pending work
        self.callbacks_pending.clear();
        self.render_pending.clear();

        // 4. Transition to ShuttingDown
        self.state = UIState::ShuttingDown;
    }

    /// Process the current event and complete it.
    ///
    /// This handles Input, Resize, CreateTerminal, DestroyTerminal events.
    /// Render and RequestCallback events require separate completion calls.
    ///
    /// # TLA+ Correspondence
    /// This implements ProcessInputResize, ProcessCreateTerminal, ProcessDestroyTerminal,
    /// ProcessRender, ProcessRequestCallback, ProcessShutdown from the TLA+ spec.
    pub fn complete_processing(&mut self) -> UIResult<()> {
        if self.state != UIState::Processing {
            return Err(UIError::InvalidStateTransition);
        }

        let event = self.current_event.take().ok_or(UIError::NoEventPending)?;

        match event.kind {
            EventKind::Input | EventKind::Resize => {
                // Simple completion - back to Idle
                self.processed_count += 1;
                self.state = UIState::Idle;
            }
            EventKind::CreateTerminal => {
                // Activate the terminal, but only if it's still Inactive.
                // DisposedMonotonic: once Disposed, a terminal stays Disposed.
                if let Some(id) = event.terminal {
                    if self.terminal_state(id) == TerminalState::Inactive {
                        self.terminal_states.insert(id, TerminalState::Active);
                    }
                    // If Disposed, silently skip (this shouldn't happen with proper validation,
                    // but we enforce it here as a defensive measure)
                }
                self.processed_count += 1;
                self.state = UIState::Idle;
            }
            EventKind::DestroyTerminal => {
                // Dispose the terminal (irreversible)
                if let Some(id) = event.terminal {
                    self.terminal_states.insert(id, TerminalState::Disposed);
                }
                self.processed_count += 1;
                self.state = UIState::Idle;
            }
            EventKind::Render => {
                // Add to render pending set
                if let Some(id) = event.terminal {
                    self.render_pending.insert(id);
                }
                self.processed_count += 1;
                self.state = UIState::Rendering;
            }
            EventKind::RequestCallback => {
                // Add callback to pending set
                if let Some(cb) = event.callback {
                    self.callbacks_pending.insert(cb);
                }
                self.processed_count += 1;
                self.state = UIState::WaitingForCallback;
            }
            EventKind::Shutdown => {
                // Delegate to single source of truth
                self.execute_shutdown();
            }
        }

        Ok(())
    }

    /// Complete a render for a terminal.
    ///
    /// # TLA+ Correspondence
    /// This implements the `CompleteRender` action from the TLA+ spec.
    pub fn complete_render(&mut self, terminal: TerminalId) -> UIResult<()> {
        if self.state != UIState::Rendering {
            return Err(UIError::InvalidStateTransition);
        }

        if !self.render_pending.remove(&terminal) {
            return Err(UIError::InvalidTerminalId);
        }

        // Transition back to Idle if no more renders pending
        if self.render_pending.is_empty() {
            self.state = UIState::Idle;
        }

        Ok(())
    }

    /// Complete a callback.
    ///
    /// # TLA+ Correspondence
    /// This implements the `CompleteCallback` action from the TLA+ spec.
    pub fn complete_callback(&mut self, callback: CallbackId) -> UIResult<()> {
        if self.state != UIState::WaitingForCallback {
            return Err(UIError::InvalidStateTransition);
        }

        if !self.callbacks_pending.remove(&callback) {
            return Err(UIError::DuplicateCallback);
        }

        // Transition back to Idle if no more callbacks pending
        if self.callbacks_pending.is_empty() {
            self.state = UIState::Idle;
        }

        Ok(())
    }

    /// Handle an event in one shot (enqueue + process + complete).
    ///
    /// This is a convenience method for simple event handling.
    pub fn handle_event(&mut self, event: Event) -> UIResult<()> {
        // If not idle, we can only enqueue
        if self.state != UIState::Idle {
            return self.enqueue(event);
        }

        // Validate and process immediately
        self.validate_event(&event)?;
        self.received_count += 1;

        // Process based on kind
        match event.kind {
            EventKind::Input | EventKind::Resize => {
                self.processed_count += 1;
            }
            EventKind::CreateTerminal => {
                // Activate the terminal, but only if it's still Inactive.
                // DisposedMonotonic: once Disposed, a terminal stays Disposed.
                if let Some(id) = event.terminal {
                    if self.terminal_state(id) == TerminalState::Inactive {
                        self.terminal_states.insert(id, TerminalState::Active);
                    }
                }
                self.processed_count += 1;
            }
            EventKind::DestroyTerminal => {
                if let Some(id) = event.terminal {
                    self.terminal_states.insert(id, TerminalState::Disposed);
                }
                self.processed_count += 1;
            }
            EventKind::Render => {
                if let Some(id) = event.terminal {
                    self.render_pending.insert(id);
                }
                self.processed_count += 1;
                self.state = UIState::Rendering;
            }
            EventKind::RequestCallback => {
                if let Some(cb) = event.callback {
                    self.callbacks_pending.insert(cb);
                }
                self.processed_count += 1;
                self.state = UIState::WaitingForCallback;
            }
            EventKind::Shutdown => {
                // Delegate to single source of truth
                self.execute_shutdown();
            }
        }

        Ok(())
    }
}

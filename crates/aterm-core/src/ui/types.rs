// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! UI Bridge types — state enums, events, and errors.
//!
//! Extracted from `ui/mod.rs` for file size management.

use super::{CallbackId, EventId, TerminalId, next_event_id};

/// UI state machine states (matches TLA+ spec exactly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
#[derive(Default)]
pub enum UIState {
    /// No work in progress, ready to process events.
    #[default]
    Idle,
    /// Currently processing an event.
    Processing,
    /// Waiting for render completion.
    Rendering,
    /// Waiting for callback completion.
    WaitingForCallback,
    /// System is shutting down.
    ShuttingDown,
}

/// Terminal state (matches TLA+ spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
#[derive(Default)]
pub enum TerminalState {
    /// Terminal slot is available.
    #[default]
    Inactive,
    /// Terminal is active and usable.
    Active,
    /// Terminal has been disposed (cannot be reactivated).
    Disposed,
}

/// Event kinds (matches TLA+ spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub enum EventKind {
    /// User input to a terminal.
    Input,
    /// Terminal resize request.
    Resize,
    /// Render request for a terminal.
    Render,
    /// Create a new terminal.
    CreateTerminal,
    /// Destroy an existing terminal.
    DestroyTerminal,
    /// Request a callback.
    RequestCallback,
    /// System shutdown.
    Shutdown,
}

/// An event in the UI system.
#[derive(Debug, Clone)]
pub struct Event {
    /// Unique event identifier.
    pub id: EventId,
    /// Type of event.
    pub kind: EventKind,
    /// Target terminal (if applicable).
    pub terminal: Option<TerminalId>,
    /// Associated callback (if applicable).
    pub callback: Option<CallbackId>,
    /// Event payload data.
    pub data: EventData,
}

/// Event-specific payload data.
#[derive(Debug, Clone, Default)]
pub struct EventData {
    /// New row count (for Resize events).
    pub rows: u16,
    /// New column count (for Resize events).
    pub cols: u16,
}

// NOTE(#2368): Event constructors must stay `pub` — they are used by the
// fuzz target `fuzz/fuzz_targets/ui_bridge.rs` (external crate boundary).
impl Event {
    /// Create an input event.
    pub fn input(terminal: TerminalId) -> Self {
        Self {
            id: next_event_id(),
            kind: EventKind::Input,
            terminal: Some(terminal),
            callback: None,
            data: EventData::default(),
        }
    }

    /// Create a resize event.
    pub fn resize(terminal: TerminalId, rows: u16, cols: u16) -> Self {
        Self {
            id: next_event_id(),
            kind: EventKind::Resize,
            terminal: Some(terminal),
            callback: None,
            data: EventData { rows, cols },
        }
    }

    /// Create a render event.
    pub fn render(terminal: TerminalId) -> Self {
        Self {
            id: next_event_id(),
            kind: EventKind::Render,
            terminal: Some(terminal),
            callback: None,
            data: EventData::default(),
        }
    }

    /// Create a create terminal event.
    pub fn create_terminal(terminal: TerminalId) -> Self {
        Self {
            id: next_event_id(),
            kind: EventKind::CreateTerminal,
            terminal: Some(terminal),
            callback: None,
            data: EventData::default(),
        }
    }

    /// Create a destroy terminal event.
    pub fn destroy_terminal(terminal: TerminalId) -> Self {
        Self {
            id: next_event_id(),
            kind: EventKind::DestroyTerminal,
            terminal: Some(terminal),
            callback: None,
            data: EventData::default(),
        }
    }

    /// Create a callback request event.
    pub fn request_callback(terminal: TerminalId, callback: CallbackId) -> Self {
        Self {
            id: next_event_id(),
            kind: EventKind::RequestCallback,
            terminal: Some(terminal),
            callback: Some(callback),
            data: EventData::default(),
        }
    }

    /// Create a shutdown event.
    pub fn shutdown() -> Self {
        Self {
            id: next_event_id(),
            kind: EventKind::Shutdown,
            terminal: None,
            callback: None,
            data: EventData::default(),
        }
    }
}

/// Error types for UI Bridge operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, aterm_error::Error)]
#[non_exhaustive]
pub enum UIError {
    /// Event queue is full.
    #[error("UI event queue is full")]
    QueueFull,
    /// System is shutting down, no new events accepted.
    #[error("system is shutting down")]
    ShuttingDown,
    /// Terminal ID is invalid or out of range.
    #[error("invalid terminal ID")]
    InvalidTerminalId,
    /// Terminal is not in the expected state.
    #[error("invalid terminal state")]
    InvalidTerminalState,
    /// Callback ID is already pending.
    #[error("duplicate callback ID")]
    DuplicateCallback,
    /// No event to process.
    #[error("no event pending")]
    NoEventPending,
    /// Invalid state transition.
    #[error("invalid state transition")]
    InvalidStateTransition,
}

/// Result type for UI Bridge operations.
pub(crate) type UIResult<T> = Result<T, UIError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_error_display_and_error_trait() {
        let err = UIError::QueueFull;
        assert_eq!(err.to_string(), "UI event queue is full");

        // Verify std::error::Error is implemented (enables ? in anyhow contexts).
        let dyn_err: &dyn std::error::Error = &err;
        assert!(dyn_err.source().is_none());
    }

    #[test]
    fn ui_error_all_variants_have_display() {
        let variants = [
            UIError::QueueFull,
            UIError::ShuttingDown,
            UIError::InvalidTerminalId,
            UIError::InvalidTerminalState,
            UIError::DuplicateCallback,
            UIError::NoEventPending,
            UIError::InvalidStateTransition,
        ];
        for v in &variants {
            assert!(!v.to_string().is_empty(), "{v:?} has empty Display");
        }
    }
}

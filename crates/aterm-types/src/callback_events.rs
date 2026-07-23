// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Normalized event types for protocol callback consumers.
//!
//! These enums provide a stable, borrowed-data view of internal protocol events
//! (tmux notifications and SSH conductor events), insulating consumers from
//! changes to the underlying protocol types.
//!
//! Extracted from aterm-core's callback normalization layer as part of
//! #5663 Phase 1 type migration.

/// Normalized tmux callback payload that avoids exposing tmux internals to consumers.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TmuxCallbackEvent<'a> {
    /// Pane output notification.
    Output {
        /// Pane identifier.
        pane_id: u32,
        /// UTF-8 payload emitted by the pane.
        data: &'a str,
    },
    /// Extended pane output notification with latency metadata.
    ExtendedOutput {
        /// Pane identifier.
        pane_id: u32,
        /// End-to-end latency in milliseconds reported by tmux.
        latency_ms: u32,
        /// UTF-8 payload emitted by the pane.
        data: &'a str,
    },
    /// Window layout changed.
    LayoutChange {
        /// Window identifier.
        window_id: u32,
        /// Raw tmux layout string.
        layout: &'a str,
    },
    /// Window created.
    WindowAdd {
        /// Window identifier.
        window_id: u32,
    },
    /// Window closed.
    WindowClose {
        /// Window identifier.
        window_id: u32,
    },
    /// Window renamed.
    WindowRenamed {
        /// Window identifier.
        window_id: u32,
        /// New window name.
        name: &'a str,
    },
    /// Active session changed.
    SessionChanged {
        /// Session identifier.
        session_id: u32,
        /// Session name.
        name: &'a str,
    },
    /// Session list changed.
    SessionsChanged,
    /// Active pane within a window changed.
    WindowPaneChanged {
        /// Window identifier.
        window_id: u32,
        /// Pane identifier.
        pane_id: u32,
    },
    /// Unlinked window created.
    UnlinkedWindowAdd {
        /// Window identifier.
        window_id: u32,
    },
    /// Unlinked window closed.
    UnlinkedWindowClose {
        /// Window identifier.
        window_id: u32,
    },
    /// Pane output paused.
    Pause {
        /// Pane identifier.
        pane_id: u32,
    },
    /// Pane output resumed.
    Continue {
        /// Pane identifier.
        pane_id: u32,
    },
    /// tmux client exited.
    Exit {
        /// Optional exit reason if provided by tmux.
        reason: Option<&'a str>,
    },
    /// tmux subscription value changed.
    SubscriptionChanged {
        /// Subscription name.
        name: &'a str,
        /// Updated subscription value.
        value: &'a str,
    },
    /// tmux client detached.
    ClientDetached {
        /// Detached client name.
        client_name: &'a str,
    },
    /// Client switched to a different session.
    ClientSessionChanged {
        /// Client name.
        client_name: &'a str,
        /// New session identifier.
        session_id: u32,
        /// New session name.
        session_name: &'a str,
    },
    /// Pane mode changed (for example copy mode entered/exited).
    PaneModeChanged {
        /// Pane identifier.
        pane_id: u32,
    },
    /// tmux notification not mapped to a stable callback variant.
    Other,
}

/// Normalized SSH conductor callback payload for consumers outside ssh_conductor internals.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SshConductorCallbackEvent<'a> {
    /// Conductor initialization event.
    Init,
    /// Command execution began.
    Begin {
        /// Command or request identifier.
        id: &'a str,
    },
    /// Command execution ended.
    End {
        /// Command or request identifier.
        id: &'a str,
    },
    /// Informational text line.
    Line {
        /// Line content.
        content: &'a str,
    },
    /// Command output chunk.
    Output {
        /// Command or request identifier.
        id: &'a str,
        /// Remote process identifier.
        pid: i32,
        /// Output channel (stdout/stderr).
        channel: i8,
        /// Nesting depth for structured output.
        depth: i32,
        /// Raw output bytes.
        data: &'a [u8],
    },
    /// Autopoll data chunk.
    Autopoll {
        /// Command or request identifier.
        id: &'a str,
        /// Raw autopoll payload.
        data: &'a [u8],
    },
    /// Conductor notification payload.
    Notification {
        /// Command or request identifier.
        id: &'a str,
        /// Notification content.
        content: &'a str,
    },
    /// Conductor terminated.
    Terminate {
        /// Termination code.
        code: i32,
    },
    /// Parse error while reading conductor output.
    ParseError {
        /// Error message.
        message: &'a str,
    },
}

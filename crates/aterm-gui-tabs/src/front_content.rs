// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authoritative focused-content identity and optional terminal handles.
//!
//! A native tab must never inherit the terminal handles that happened to be
//! active before it.  [`FrontContent`] names the focused leaf; a
//! [`TerminalMirror`] exists if and only if that leaf is terminal. Terminal-only
//! operations resolve through this optional capability; native content owns no
//! placeholder terminal, invalid fd, sink, or session identity.

use std::sync::{Arc, Mutex};

use aterm_session::sink::SinkWriter;

use crate::tab_model::{AppInstanceId, ViewId};
use aterm_core::terminal::Terminal;

/// Stable identity of the focused content leaf in one window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrontContent {
    Terminal {
        view: ViewId,
        session: u64,
    },
    Native {
        instance: AppInstanceId,
        view: ViewId,
    },
}

impl FrontContent {
    #[must_use]
    #[inline]
    pub const fn view(self) -> ViewId {
        match self {
            Self::Terminal { view, .. } | Self::Native { view, .. } => view,
        }
    }

    #[must_use]
    #[inline]
    pub const fn native(self) -> Option<(AppInstanceId, ViewId)> {
        match self {
            Self::Native { instance, view } => Some((instance, view)),
            Self::Terminal { .. } => None,
        }
    }

    /// The terminal SESSION behind this front, or `None` for native content —
    /// the identity half of [`Self::native`]. Callers that need the session id
    /// without a capability (the DEC 1004 focus-report account, which must name
    /// a session it can no longer reach through the window) read it here rather
    /// than re-matching the enum.
    #[must_use]
    #[inline]
    pub const fn terminal_session(self) -> Option<u64> {
        match self {
            Self::Terminal { session, .. } => Some(session),
            Self::Native { .. } => None,
        }
    }

    #[must_use]
    #[inline]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal { .. })
    }

    #[must_use]
    #[inline]
    pub const fn is_native(self) -> bool {
        matches!(self, Self::Native { .. })
    }
}

/// Handles for the focused terminal leaf only.
///
/// This is deliberately separate from [`FrontContent`]: identity is cheap and
/// always available, while terminal handles are capability-bearing resources.
#[derive(Clone)]
pub struct TerminalMirror {
    pub session: u64,
    pub term: Arc<Mutex<Terminal>>,
    pub master: i32,
    pub sink: Arc<SinkWriter>,
    /// The session's UI-waiter register (`SessionCtx::ui_waiting`), so the
    /// redraw's blocking acquisitions can announce themselves to the reader.
    pub ui_waiting: Arc<std::sync::atomic::AtomicU32>,
}

impl std::fmt::Debug for TerminalMirror {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalMirror")
            .field("session", &self.session)
            .field("master", &self.master)
            .finish_non_exhaustive()
    }
}

/// Which window layer currently owns keyboard focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowFocus {
    Host,
    Content(ViewId),
}

impl WindowFocus {
    #[must_use]
    #[inline]
    pub const fn is_host(self) -> bool {
        matches!(self, Self::Host)
    }

    #[must_use]
    #[inline]
    pub const fn focused_view(self) -> Option<ViewId> {
        match self {
            Self::Host => None,
            Self::Content(v) => Some(v),
        }
    }
}

/// Explicit native-view lifecycle. Only adjacent, forward transitions are
/// accepted; stale async work cannot remount a closing or closed view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewLifecycle {
    #[default]
    Created,
    Mounted,
    Suspended,
    Closing,
    Closed,
}

impl ViewLifecycle {
    #[must_use]
    #[inline]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Created, Self::Mounted)
                | (Self::Mounted, Self::Suspended | Self::Closing)
                | (Self::Suspended, Self::Mounted | Self::Closing)
                | (Self::Closing, Self::Closed)
        )
    }

    #[must_use]
    #[inline]
    pub const fn is_terminal_state(self) -> bool {
        matches!(self, Self::Closed)
    }

    #[must_use]
    #[inline]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Mounted | Self::Suspended)
    }

    #[inline]
    pub const fn transition(&mut self, next: Self) -> bool {
        let allowed = self.can_transition_to(next);
        if allowed {
            *self = next;
        }
        allowed
    }
}

/// Encode a `(tab_id, view_id)` pair into 16 little-endian bytes.
#[must_use]
#[inline]
pub const fn encode_tab_view_pair_le(tab_id: u64, view_id: u64) -> [u8; 16] {
    [
        tab_id as u8,
        (tab_id >> 8) as u8,
        (tab_id >> 16) as u8,
        (tab_id >> 24) as u8,
        (tab_id >> 32) as u8,
        (tab_id >> 40) as u8,
        (tab_id >> 48) as u8,
        (tab_id >> 56) as u8,
        view_id as u8,
        (view_id >> 8) as u8,
        (view_id >> 16) as u8,
        (view_id >> 24) as u8,
        (view_id >> 32) as u8,
        (view_id >> 40) as u8,
        (view_id >> 48) as u8,
        (view_id >> 56) as u8,
    ]
}

/// Decode 16 little-endian bytes into a `(tab_id, view_id)` pair.
#[must_use]
#[inline]
pub const fn decode_tab_view_pair_le(bytes: [u8; 16]) -> (u64, u64) {
    let tab_id = (bytes[0] as u64)
        | ((bytes[1] as u64) << 8)
        | ((bytes[2] as u64) << 16)
        | ((bytes[3] as u64) << 24)
        | ((bytes[4] as u64) << 32)
        | ((bytes[5] as u64) << 40)
        | ((bytes[6] as u64) << 48)
        | ((bytes[7] as u64) << 56);
    let view_id = (bytes[8] as u64)
        | ((bytes[9] as u64) << 8)
        | ((bytes[10] as u64) << 16)
        | ((bytes[11] as u64) << 24)
        | ((bytes[12] as u64) << 32)
        | ((bytes[13] as u64) << 40)
        | ((bytes[14] as u64) << 48)
        | ((bytes[15] as u64) << 56);
    (tab_id, view_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_is_forward_only_and_suspend_is_reversible() {
        let mut state = ViewLifecycle::Created;
        assert!(state.transition(ViewLifecycle::Mounted));
        assert!(state.is_active());
        assert!(state.transition(ViewLifecycle::Suspended));
        assert!(state.transition(ViewLifecycle::Mounted));
        assert!(state.transition(ViewLifecycle::Closing));
        assert!(!state.transition(ViewLifecycle::Mounted));
        assert!(state.transition(ViewLifecycle::Closed));
        assert!(state.is_terminal_state());
        assert!(!state.transition(ViewLifecycle::Created));
        let pair = encode_tab_view_pair_le(11, 22);
        assert_eq!(decode_tab_view_pair_le(pair), (11, 22));
    }
}

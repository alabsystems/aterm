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

use crate::Terminal;
use crate::tab_model::{AppInstanceId, ViewId};

/// Stable identity of the focused content leaf in one window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrontContent {
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
    pub(crate) const fn view(self) -> ViewId {
        match self {
            Self::Terminal { view, .. } | Self::Native { view, .. } => view,
        }
    }

    #[must_use]
    pub(crate) const fn native(self) -> Option<(AppInstanceId, ViewId)> {
        match self {
            Self::Native { instance, view } => Some((instance, view)),
            Self::Terminal { .. } => None,
        }
    }
}

/// Handles for the focused terminal leaf only.
///
/// This is deliberately separate from [`FrontContent`]: identity is cheap and
/// always available, while terminal handles are capability-bearing resources.
#[derive(Clone)]
pub(crate) struct TerminalMirror {
    pub(crate) session: u64,
    pub(crate) term: Arc<Mutex<Terminal>>,
    pub(crate) master: i32,
    pub(crate) sink: Arc<SinkWriter>,
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
#[allow(
    dead_code,
    reason = "host and overlay focus states land as their event paths migrate to WindowFocus"
)]
pub(crate) enum WindowFocus {
    Host,
    Content(ViewId),
    Overlay,
}

/// Explicit native-view lifecycle. Only adjacent, forward transitions are
/// accepted; stale async work cannot remount a closing or closed view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "lifecycle contract is consumed incrementally by mixed-leaf mounting and restore"
)]
pub(crate) enum ViewLifecycle {
    #[default]
    Created,
    Mounted,
    Suspended,
    Closing,
    Closed,
}

impl ViewLifecycle {
    pub(crate) fn transition(&mut self, next: Self) -> bool {
        let allowed = matches!(
            (*self, next),
            (Self::Created, Self::Mounted)
                | (Self::Mounted, Self::Suspended | Self::Closing)
                | (Self::Suspended, Self::Mounted | Self::Closing)
                | (Self::Closing, Self::Closed)
        );
        if allowed {
            *self = next;
        }
        allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_is_forward_only_and_suspend_is_reversible() {
        let mut state = ViewLifecycle::Created;
        assert!(state.transition(ViewLifecycle::Mounted));
        assert!(state.transition(ViewLifecycle::Suspended));
        assert!(state.transition(ViewLifecycle::Mounted));
        assert!(state.transition(ViewLifecycle::Closing));
        assert!(!state.transition(ViewLifecycle::Mounted));
        assert!(state.transition(ViewLifecycle::Closed));
        assert!(!state.transition(ViewLifecycle::Created));
    }
}

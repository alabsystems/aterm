// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host capability and session-access traits for control verbs.

use std::borrow::Cow;
use std::time::Duration;

use aterm_core::terminal::Terminal;

/// Host capabilities on which control verbs gate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostCapabilities {
    /// Whether pixels can be captured (`image`/`video`/`window`/`chrome`).
    pub frame_source: bool,
    /// Whether a windowing event loop exists to repaint and route synthetic input.
    pub event_loop: bool,
    /// Whether the system clipboard is reachable (`copy`).
    pub clipboard: bool,
    /// Whether a session roster is kept for [`SessionHost::sessions`] and [`SessionHost::resolve`].
    pub roster: bool,
}

/// Session lifecycle state in the `sessions` wire vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// Registered with live engine and input sink before reader confirmation.
    Spawning,
    /// Live session feeding the engine.
    Alive,
    /// Command exited while the engine remains readable.
    Exited,
}

/// One row of [`SessionHost::sessions`] matching a `sessions`/`ls` wire line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEntry {
    /// Process-local session ID.
    pub sid: u64,
    /// Stable fabric identity (`s-<hex>`).
    pub id: String,
    /// Spawning session's stable ID, if any.
    pub parent: Option<String>,
    /// Session lifecycle state.
    pub state: SessionState,
    /// Best-effort live title.
    pub title: String,
    /// Whether any user metadata is set (`meta=<1|0>`).
    pub has_meta: bool,
}

/// Wire `@<selector>` target resolved against the session roster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selector<'a> {
    /// `@<n>` process-local session ID.
    Local(u64),
    /// `@s-<hex>` stable fabric ID.
    Id(&'a str),
}

impl<'a> From<Selector<'a>> for aterm_types::control_request::Selector<'a> {
    /// The wire's reading of a roster selector, so a host resolves it through the
    /// lookup the dispatcher resolves `@<body>` through: an `Id` that is no stable
    /// id's spelling names no session.
    fn from(selector: Selector<'a>) -> Self {
        use aterm_types::control_request::{Selector as Wire, Sid};
        match selector {
            Selector::Local(n) => Wire::Local(n),
            Selector::Id(id) => Sid::parse(id).map_or(Wire::Invalid(id), Wire::Sid),
        }
    }
}

impl<'a> Selector<'a> {
    /// Return the local sid if `self` is [`Selector::Local`].
    #[must_use]
    #[inline]
    pub const fn local_sid(self) -> Option<u64> {
        match self {
            Self::Local(sid) => Some(sid),
            Self::Id(_) => None,
        }
    }

    /// Return the stable id string if `self` is [`Selector::Id`].
    #[must_use]
    #[inline]
    pub const fn stable_id(self) -> Option<&'a str> {
        match self {
            Self::Local(_) => None,
            Self::Id(id) => Some(id),
        }
    }
}

/// Blocking wake handle returned by [`SessionHost::subscribe`].
pub trait ChangeWait {
    /// Parks until the session changes, `timeout` elapses, or [`Self::caller_gone`] is true.
    fn wait(&self, timeout: Duration) -> bool;

    /// Whether the client that requested this wait has disconnected.
    fn caller_gone(&self) -> bool {
        false
    }
}

/// Host of one or more terminal sessions addressed by process-local `sid`.
///
/// # The roster answers fleet-wide; every other method fails closed on a foreign sid
///
/// [`SessionHost::sessions`] and [`SessionHost::resolve`] answer for the whole
/// roster. Per-session methods answer only for sids this host serves (possibly
/// one: `aterm-gui`'s host is session-scoped). A sid it does not serve gets
/// `None` (a no-op for returnless methods), never its own session under a
/// borrowed number, which keeps the cross-host sequence safe:
///
/// ```
/// use aterm_control::{Selector, SessionHost};
///
/// fn clear_selected(host: &impl SessionHost, sel: Selector<'_>) -> Option<()> {
///     let sid = host.resolve(sel)?;  // fleet-wide answer
///     host.with_terminal_mut(sid, |t| t.text_selection_mut().clear())
///                                    // None unless THIS host serves that sid
/// }
/// ```
pub trait SessionHost {
    /// Capabilities supported by this host.
    fn capabilities(&self) -> HostCapabilities;

    /// Returns all sessions served by this host, ordered by ascending `sid`.
    fn sessions(&self) -> Vec<SessionEntry>;

    /// Resolves `@<selector>` against the roster.
    fn resolve(&self, selector: Selector<'_>) -> Option<u64>;

    /// Runs `f` against session `sid`'s terminal if served by this host; a host may first wait, bounded, for an unfinished DEC 2026 frame.
    fn with_terminal<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R>;

    /// Runs `f` with mutable access to session `sid`'s terminal if served by this host.
    fn with_terminal_mut<R>(&self, sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R>;

    /// [`SessionHost::with_terminal`] as the terminal stands, never waiting for a frame (`blocks`, `wait`).
    fn with_terminal_as_it_stands<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
        self.with_terminal(sid, f)
    }

    /// Requests a repaint for `sid` if served by this host.
    fn request_redraw(&self, sid: u64);

    /// Subscribes to changes on `sid` for the lifetime of the returned handle.
    fn subscribe(&self, sid: u64) -> Box<dyn ChangeWait + '_>;

    /// Returns whether session `sid` has ended.
    fn session_ended(&self, sid: u64) -> bool {
        let _ = sid;
        false
    }

    /// Writes `text` to the system clipboard.
    fn clipboard_set(&self, text: &str) -> bool;

    /// Converts a shell-reported working directory into the host platform's native path format.
    fn native_cwd<'p>(&self, reported: &'p str) -> Cow<'p, str> {
        Cow::Borrowed(reported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A roster selector reads as the wire grammar reads its body: a local sid
    /// stays one, an `Id` spelled as a stable id is that id, and any other `Id`
    /// names no session.
    #[test]
    fn selector_reads_as_the_wire_grammar() {
        use aterm_types::control_request::{Selector as Wire, Sid};
        assert_eq!(Wire::from(Selector::Local(12)), Wire::Local(12));
        let sid = Sid::parse("s-0f1e").expect("a stable id");
        assert_eq!(Wire::from(Selector::Id("s-0f1e")), Wire::Sid(sid));
        assert_eq!(Wire::from(Selector::Id("S-0F1E")), Wire::Invalid("S-0F1E"));
        assert_eq!(Selector::Local(12).local_sid(), Some(12));
        assert_eq!(Selector::Id("s-0f1e").local_sid(), None);
        assert_eq!(Selector::Id("s-0f1e").stable_id(), Some("s-0f1e"));
        assert_eq!(Selector::Local(12).stable_id(), None);
    }
}

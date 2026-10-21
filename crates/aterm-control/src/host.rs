// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The seam: what a verb body needs from whatever is hosting the session.

use std::borrow::Cow;
use std::time::Duration;

use aterm_core::terminal::Terminal;

/// What a host can do. Verbs gate on these, so a host lacking a facility
/// answers `ERR unsupported` instead of a plausible empty answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostCapabilities {
    /// Pixels can be captured (`image`/`video`/`window`/`chrome`).
    pub frame_source: bool,
    /// A windowing event loop exists to repaint and to route synthetic input.
    pub event_loop: bool,
    /// A system clipboard is reachable (`copy`).
    pub clipboard: bool,
    /// A roster is kept, so [`SessionHost::sessions`] and
    /// [`SessionHost::resolve`] answer for real. `false` says "I keep no
    /// index", which an empty `Vec` alone could not distinguish from "no
    /// sessions".
    pub roster: bool,
}

impl HostCapabilities {
    /// Headless daemon capabilities (`roster` enabled, no windowing or clipboard).
    #[must_use]
    pub const fn headless_daemon() -> Self {
        Self {
            frame_source: false,
            event_loop: false,
            clipboard: false,
            roster: true,
        }
    }

    /// Full GUI capabilities (all 4 facilities enabled).
    #[must_use]
    pub const fn full_gui() -> Self {
        Self {
            frame_source: true,
            event_loop: true,
            clipboard: true,
            roster: true,
        }
    }

    /// Whether the host can capture rendered window frames (`frame_source && event_loop`).
    #[must_use]
    pub const fn can_capture_frames(self) -> bool {
        self.frame_source && self.event_loop
    }

    /// Count of enabled host facilities (`0..=4`).
    #[must_use]
    pub const fn enabled_count(self) -> u8 {
        (self.frame_source as u8)
            + (self.event_loop as u8)
            + (self.clipboard as u8)
            + (self.roster as u8)
    }
}

/// A session's lifecycle as its host observes it: the `sessions` wire
/// vocabulary, onto which each host maps its own states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// Registered, engine + input sink live, reader not yet confirmed.
    Spawning,
    /// Live: something is feeding the engine.
    Alive,
    /// The command exited; the engine is still readable.
    Exited,
}

impl SessionState {
    /// Wire token for this session state (`"spawning"`, `"alive"`, or `"exited"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spawning => "spawning",
            Self::Alive => "alive",
            Self::Exited => "exited",
        }
    }

    /// Whether the session's input sink can accept bytes (`Spawning` or `Alive`).
    #[must_use]
    pub const fn can_accept_input(self) -> bool {
        matches!(self, Self::Spawning | Self::Alive)
    }

    /// Whether the session has exited.
    #[must_use]
    pub const fn is_exited(self) -> bool {
        matches!(self, Self::Exited)
    }
}

/// One row of [`SessionHost::sessions`]: exactly the fields a `sessions`/`ls`
/// line carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEntry {
    /// Process-local id: the sid every other method on this trait takes.
    pub sid: u64,
    /// Stable fabric identity (`s-<hex>`) — the `@<id>` selector form, and what
    /// survives a restart that renumbers `sid`.
    pub id: String,
    /// The spawning session's stable id, if any (the family tree).
    pub parent: Option<String>,
    /// Lifecycle; see [`SessionState`].
    pub state: SessionState,
    /// Live title, best effort.
    pub title: String,
    /// Whether any user metadata is set (the wire's `meta=<1|0>`), so a fleet
    /// driver knows which sessions to query without N round trips.
    pub has_meta: bool,
}

/// A wire `@<selector>` target the roster can answer. `@.` (self) is absent:
/// self is the connection's own session, which the dispatcher knows and the
/// roster does not. So is a bare `@`, which names nothing: the dispatcher
/// refuses it (`aterm_types::control_verbs::EMPTY_SELECTOR`), never reading
/// it as self. The wire's selector is read by the one request grammar
/// ([`aterm_types::control_request::Selector`]); an `Id` is a body that
/// grammar reads as a stable id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selector<'a> {
    /// `@<n>` — the process-local sid.
    Local(u64),
    /// `@s-<hex>` — the stable fabric id.
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
    pub const fn local_sid(self) -> Option<u64> {
        match self {
            Self::Local(sid) => Some(sid),
            Self::Id(_) => None,
        }
    }

    /// Return the stable id string if `self` is [`Selector::Id`].
    #[must_use]
    pub const fn stable_id(self) -> Option<&'a str> {
        match self {
            Self::Local(_) => None,
            Self::Id(id) => Some(id),
        }
    }
}

/// Blocking wake handle from [`SessionHost::subscribe`].
///
/// A handle rather than a `wait(sid, timeout)` method because `wait` must be
/// registered before it re-checks the entry snapshot, so a completion landing
/// in that gap still leaves a notify pending.
pub trait ChangeWait {
    /// Park until the session changes, or until `timeout` elapses. `true` on a
    /// wake, `false` on timeout, or earlier once [`Self::caller_gone`] would
    /// answer `true`. A spurious/coalesced wake is fine — every caller
    /// re-reads the state it is waiting on.
    fn wait(&self, timeout: Duration) -> bool;

    /// Whether the client that asked for this wait has hung up. A socket host
    /// checks its connection, and parks in slices short enough to notice; the
    /// default is never gone. Waiting verbs return their lane when `true`.
    fn caller_gone(&self) -> bool {
        false
    }
}

/// A host of one or more terminal sessions, addressed by process-local sid.
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
///
/// Resolve to pick a target, then obtain a host bound to it.
///
/// Not object-safe: the `impl FnOnce` accessors cost no `Box` per verb and
/// keep the host's lock guard (and `aterm-gui`'s debug lock-hold tripwire) on
/// the host's side. Both hosts dispatch statically.
pub trait SessionHost {
    /// What this host can do; see [`HostCapabilities`].
    fn capabilities(&self) -> HostCapabilities;

    /// The roster behind `sessions`/`ls`: every session this host serves,
    /// ascending by `sid`. A host serving one session returns one entry; only a
    /// host keeping no roster ([`HostCapabilities::roster`] false) may answer
    /// empty.
    fn sessions(&self) -> Vec<SessionEntry>;

    /// Resolve a `@<selector>` against the roster; `None` when it holds no such
    /// session (the caller answers `ERR no such session` rather than falling
    /// back) and for every selector on a host with no roster. The sid need not
    /// be one this host serves (see [`SessionHost`]).
    fn resolve(&self, selector: Selector<'_>) -> Option<u64>;

    /// Run `f` against session `sid`'s terminal, or `None` if this host does not
    /// serve `sid`. Closure-based so the host owns the guard type.
    fn with_terminal<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R>;

    /// [`SessionHost::with_terminal`] with mutable access.
    fn with_terminal_mut<R>(&self, sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R>;

    /// Ask the host to repaint `sid`. A no-op on a host with no event loop, and on
    /// a sid it does not serve.
    fn request_redraw(&self, sid: u64);

    /// Register interest in `sid`'s changes for the life of the returned handle
    /// (see [`ChangeWait`]). A sid this host does not serve gets a handle that
    /// only times out.
    fn subscribe(&self, sid: u64) -> Box<dyn ChangeWait + '_>;

    /// Whether session `sid` has ended — its program exited or it was closed —
    /// so a verb parked on it answers `ERR exited` rather than waiting out its
    /// deadline. The host notifies [`SessionHost::subscribe`]'s handles when it
    /// happens. Defaults to `false`: a host whose sessions outlive every verb.
    fn session_ended(&self, sid: u64) -> bool {
        let _ = sid;
        false
    }

    /// Place `text` on the system clipboard; `false` if the write failed. Only
    /// called when [`HostCapabilities::clipboard`] is set.
    fn clipboard_set(&self, text: &str) -> bool;

    /// A shell-reported working directory as this host's platform names it
    /// (what `blocks` prints for `cwd`).
    ///
    /// The engine stores the cwd as reported; for OSC 7 that is the RFC 8089
    /// URI path (`/C:/Users//x` on Windows from `file://host/C:/Users//x`), kept
    /// so recordings decode the same everywhere. The default is the identity,
    /// correct on POSIX; a Windows host converts, so a pwsh block reads
    /// `C:\Users\x` like a cmd block.
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

    #[test]
    fn host_capabilities_and_session_state_const_helpers() {
        let daemon = HostCapabilities::headless_daemon();
        assert!(daemon.roster && !daemon.can_capture_frames());
        assert_eq!(daemon.enabled_count(), 1);
        let gui = HostCapabilities::full_gui();
        assert!(gui.roster && gui.can_capture_frames());
        assert_eq!(gui.enabled_count(), 4);
        assert_eq!(SessionState::Spawning.as_str(), "spawning");
        assert!(SessionState::Alive.can_accept_input() && !SessionState::Alive.is_exited());
        assert!(SessionState::Exited.is_exited() && !SessionState::Exited.can_accept_input());
    }
}
